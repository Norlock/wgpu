//! Utility structures and functions.

mod belt;

use std::{
    borrow::Cow,
    convert::TryFrom,
    mem::{align_of, size_of},
    ptr::copy_nonoverlapping,
};

pub use belt::StagingBelt;

/// Treat the given byte slice as a SPIR-V module.
///
/// # Panic
///
/// This function panics if:
///
/// - Input length isn't multiple of 4
/// - Input is longer than [`usize::max_value`]
/// - SPIR-V magic number is missing from beginning of stream
pub fn make_spirv<'a>(data: &'a [u8]) -> super::ShaderSource<'a> {
    const MAGIC_NUMBER: u32 = 0x0723_0203;

    assert_eq!(
        data.len() % size_of::<u32>(),
        0,
        "data size is not a multiple of 4"
    );

    //If the data happens to be aligned, directly use the byte array,
    // otherwise copy the byte array in an owned vector and use that instead.
    let words = if data.as_ptr().align_offset(align_of::<u32>()) == 0 {
        let (pre, words, post) = unsafe { data.align_to::<u32>() };
        debug_assert!(pre.is_empty());
        debug_assert!(post.is_empty());
        Cow::from(words)
    } else {
        let mut words = vec![0u32; data.len() / size_of::<u32>()];
        unsafe {
            copy_nonoverlapping(data.as_ptr(), words.as_mut_ptr() as *mut u8, data.len());
        }
        Cow::from(words)
    };

    assert_eq!(
        words[0], MAGIC_NUMBER,
        "wrong magic word {:x}. Make sure you are using a binary SPIRV file.",
        words[0]
    );
    super::ShaderSource::SpirV(words)
}

/// Utility methods not meant to be in the main API.
pub trait DeviceExt {
    /// Creates a [`Buffer`] with data to initialize it.
    fn create_buffer_init(&self, desc: &BufferInitDescriptor) -> crate::Buffer;

    /// Upload an entire texture and its mipmaps from a source buffer.
    ///
    /// Expects all mipmaps to be tightly packed in the data buffer.
    ///
    /// If the texture is a 2DArray texture, uploads each layer in order, expecting
    /// each layer and its mips to be tightly packed.
    ///
    /// Example:
    /// Layer0Mip0 Layer0Mip1 Layer0Mip2 ... Layer1Mip0 Layer1Mip1 Layer1Mip2 ...  
    fn create_texture_with_data(
        &self,
        queue: &crate::Queue,
        desc: &crate::TextureDescriptor,
        data: &[u8],
    ) -> crate::Texture;
}

impl DeviceExt for crate::Device {
    fn create_buffer_init(&self, descriptor: &BufferInitDescriptor<'_>) -> crate::Buffer {
        let unpadded_size = descriptor.contents.len() as crate::BufferAddress;
        let padding = crate::COPY_BUFFER_ALIGNMENT - unpadded_size % crate::COPY_BUFFER_ALIGNMENT;
        let padded_size = padding + unpadded_size;

        let wgt_descriptor = crate::BufferDescriptor {
            label: descriptor.label,
            size: padded_size,
            usage: descriptor.usage,
            mapped_at_creation: true,
        };

        let mut map_context = crate::MapContext::new(padded_size);

        map_context.initial_range = 0..padded_size;

        let buffer = self.create_buffer(&wgt_descriptor);
        {
            let mut slice = buffer.slice(..).get_mapped_range_mut();
            slice[0..unpadded_size as usize].copy_from_slice(descriptor.contents);

            for i in unpadded_size..padded_size {
                slice[i as usize] = 0;
            }
        }
        buffer.unmap();
        buffer
    }

    fn create_texture_with_data(
        &self,
        queue: &crate::Queue,
        desc: &crate::TextureDescriptor,
        data: &[u8],
    ) -> crate::Texture {
        let texture = self.create_texture(desc);

        let format_info = desc.format.describe();

        let (layer_iterations, mip_extent) = if desc.dimension == crate::TextureDimension::D3 {
            (1, desc.size)
        } else {
            (
                desc.size.depth,
                crate::Extent3d {
                    depth: 1,
                    ..desc.size
                },
            )
        };

        let mip_level_count =
            u8::try_from(desc.mip_level_count).expect("mip level count overflows a u8");

        let mut binary_offset = 0;
        for layer in 0..layer_iterations {
            for mip in 0..mip_level_count {
                let mip_size = mip_extent.at_mip_level(mip).unwrap();

                // When uploading mips of compressed textures and the mip is supposed to be
                // a size that isn't a multiple of the block size, the mip needs to be uploaded
                // as it's "physical size" which is the size rounded up to the nearest block size.
                let mip_physical = mip_size.physical_size(desc.format);

                // All these calculations are performed on the physical size as that's the
                // data that exists in the buffer.
                let width_blocks = mip_physical.width / format_info.block_dimensions.0 as u32;
                let height_blocks = mip_physical.height / format_info.block_dimensions.1 as u32;

                let bytes_per_row = width_blocks * format_info.block_size as u32;
                let data_size = bytes_per_row * height_blocks;

                let end_offset = binary_offset + data_size as usize;

                queue.write_texture(
                    crate::TextureCopyView {
                        texture: &texture,
                        mip_level: mip as u32,
                        origin: crate::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer,
                        },
                    },
                    &data[binary_offset..end_offset],
                    crate::TextureDataLayout {
                        offset: 0,
                        bytes_per_row,
                        rows_per_image: 0,
                    },
                    mip_physical,
                );

                binary_offset = end_offset;
            }
        }

        texture
    }
}

/// Describes a [`Buffer`] when allocating.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BufferInitDescriptor<'a> {
    /// Debug label of a buffer. This will show up in graphics debuggers for easy identification.
    pub label: Option<&'a str>,
    /// Contents of a buffer on creation.
    pub contents: &'a [u8],
    /// Usages of a buffer. If the buffer is used in any way that isn't specified here, the operation
    /// will panic.
    pub usage: crate::BufferUsage,
}
