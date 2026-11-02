//! CPU-side image/layout guards, shared by uploads and viewport targets. Keep these independent of
//! a GPU so malformed input and device-limit regressions run even on machines without an adapter.

use crate::{AiCoverage, LinearImage, PipelineError};

pub(crate) fn output_layout(
    width: u32,
    height: u32,
    max_dimension: u32,
    max_buffer_size: u64,
) -> Result<(u32, u64), PipelineError> {
    if width == 0 || height == 0 {
        return Err(PipelineError::InvalidInput("dimensions must be nonzero"));
    }
    if width > max_dimension || height > max_dimension {
        return Err(PipelineError::ImageTooLarge {
            w: width,
            h: height,
            max: max_dimension,
        });
    }
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_row = width
        .checked_mul(4)
        .and_then(|row| row.checked_add(alignment - 1))
        .map(|row| row / alignment * alignment)
        .ok_or(PipelineError::InvalidInput("row size overflows"))?;
    let readback_size = u64::from(padded_row) * u64::from(height);
    if readback_size > isize::MAX as u64 {
        return Err(PipelineError::InvalidInput(
            "readback address space overflows",
        ));
    }
    if readback_size > max_buffer_size {
        return Err(PipelineError::BufferTooLarge {
            required: readback_size,
            max: max_buffer_size,
        });
    }
    Ok((padded_row, readback_size))
}

pub(crate) fn linear_image(
    image: &LinearImage,
    max_dimension: u32,
    max_buffer_size: u64,
) -> Result<usize, PipelineError> {
    output_layout(image.width, image.height, max_dimension, max_buffer_size)?;
    // Queue::write_texture uses a u32 row stride for the RGBA32Float upload.
    image
        .width
        .checked_mul(16)
        .ok_or(PipelineError::InvalidInput("upload row size overflows"))?;
    let pixels = (image.width as usize)
        .checked_mul(image.height as usize)
        .ok_or(PipelineError::InvalidInput("pixel count overflows"))?;
    let rgb_len = pixels
        .checked_mul(3)
        .ok_or(PipelineError::InvalidInput("RGB length overflows"))?;
    if image.data.len() != rgb_len {
        return Err(PipelineError::InvalidInput(
            "RGB length does not match dimensions",
        ));
    }
    if image.data.iter().any(|value| !value.is_finite()) {
        return Err(PipelineError::InvalidInput("RGB samples must be finite"));
    }
    let rgba_len = pixels
        .checked_mul(4)
        .ok_or(PipelineError::InvalidInput("RGBA length overflows"))?;
    if rgba_len
        .checked_mul(std::mem::size_of::<f32>())
        .is_none_or(|bytes| bytes > isize::MAX as usize)
    {
        return Err(PipelineError::InvalidInput(
            "RGBA allocation size overflows",
        ));
    }
    // Finite negative samples and >1 scene-linear HDR headroom are deliberately preserved.
    Ok(rgba_len)
}

pub(crate) fn ai_coverages(coverages: &[AiCoverage]) -> Result<(), PipelineError> {
    for coverage in coverages {
        if coverage.width == 0
            || coverage.height == 0
            || coverage.width > crate::AI_TEX_EDGE
            || coverage.height > crate::AI_TEX_EDGE
            || (coverage.width as usize).checked_mul(coverage.height as usize)
                != Some(coverage.alpha.len())
        {
            return Err(PipelineError::InvalidInput(
                "AI coverage dimensions/length are invalid",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_validation_preserves_hdr_and_rejects_invalid_buffers() {
        let mut image = LinearImage {
            width: 1,
            height: 1,
            data: vec![-0.01, 0.0, 32.0],
        };
        assert_eq!(linear_image(&image, 16_384, u64::MAX).unwrap(), 4);
        for bad in [
            vec![0.0; 2],
            vec![0.0; 4],
            vec![f32::NAN; 3],
            vec![f32::INFINITY; 3],
            vec![f32::NEG_INFINITY; 3],
        ] {
            image.data = bad;
            assert!(linear_image(&image, 16_384, u64::MAX).is_err());
        }
        image.width = 0;
        image.data.clear();
        assert!(linear_image(&image, 16_384, u64::MAX).is_err());
    }

    #[test]
    fn readback_accounts_for_row_padding_and_device_buffer_limit() {
        assert_eq!(output_layout(65, 3, 16_384, 1536).unwrap(), (512, 1536));
        assert!(matches!(
            output_layout(65, 3, 16_384, 1535),
            Err(PipelineError::BufferTooLarge { .. })
        ));
        assert!(matches!(
            output_layout(16_385, 1, 16_384, u64::MAX),
            Err(PipelineError::ImageTooLarge { .. })
        ));
        assert!(output_layout(1, 0, 16_384, u64::MAX).is_err());
        assert!(output_layout(u32::MAX, 1, u32::MAX, u64::MAX).is_err());
        // Large but representable dimensions must not wrap at 4 GiB during readback sizing.
        assert_eq!(
            output_layout(32_768, 32_768, 32_768, u64::MAX).unwrap().1,
            1u64 << 32
        );
    }

    #[test]
    fn malformed_ai_coverage_cannot_reach_texture_upload() {
        let mut coverage = AiCoverage {
            hash: "test".to_owned(),
            width: 2,
            height: 2,
            alpha: vec![255; 4],
        };
        assert!(ai_coverages(std::slice::from_ref(&coverage)).is_ok());
        coverage.alpha.pop();
        assert!(ai_coverages(std::slice::from_ref(&coverage)).is_err());
        coverage.width = crate::AI_TEX_EDGE + 1;
        assert!(ai_coverages(std::slice::from_ref(&coverage)).is_err());
        coverage.width = 0;
        coverage.alpha.clear();
        assert!(ai_coverages(std::slice::from_ref(&coverage)).is_err());
    }
}
