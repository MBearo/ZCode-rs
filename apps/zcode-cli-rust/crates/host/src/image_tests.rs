use super::*;

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    });
    let mut data = std::io::Cursor::new(vec![]);
    DynamicImage::ImageRgb8(image)
        .write_to(&mut data, ImageFormat::Png)
        .unwrap();
    data.into_inner()
}

#[test]
fn attachment_budget_has_no_token_limit() {
    // Read 以 25 000 token 为上限（base64 200 000 字符）；附件只受字节预算约束。
    assert!(!Budget::READ.fits(150_003));
    assert!(Budget::ATTACHMENT.fits(150_003));
    assert!(Budget::ATTACHMENT.fits(MAX_RAW));
    assert!(!Budget::ATTACHMENT.fits(MAX_RAW + 1));
}

#[test]
fn attachment_metadata_follows_node() {
    let small = png(40, 30);
    let kept = prepare(small.clone(), "image/png", Budget::ATTACHMENT).unwrap();
    assert!(kept.original);
    assert_eq!(kept.data, small);
    assert_eq!(
        kept.attachment_metadata().to_string(),
        format!(
            r#"{{"height":30,"maxDimension":2000,"originalHeight":30,"originalWidth":40,"resized":false,"transformedSizeBytes":{},"width":40}}"#,
            small.len()
        )
    );
    let wide = prepare(png(2400, 60), "image/png", Budget::ATTACHMENT).unwrap();
    assert!(wide.resized && !wide.original);
    let meta = wide.attachment_metadata();
    assert_eq!(
        (meta["width"].as_u64(), meta["originalWidth"].as_u64()),
        (Some(2000), Some(2400))
    );
    assert_eq!(meta["transformedSizeBytes"], wide.data.len());
    // WebP 原样通过：Node 的尺寸成员为 undefined，JSON 中省略。
    let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
    webp.resize(64, 0);
    let passed = prepare(webp, "image/png", Budget::ATTACHMENT).unwrap();
    assert_eq!(passed.mime, "image/webp");
    assert_eq!(
        passed.attachment_metadata().to_string(),
        r#"{"maxDimension":2000,"resized":false,"transformedSizeBytes":64}"#
    );
}

#[test]
fn mime_from_path_defaults_to_png() {
    assert_eq!(mime_from_path("/a/B.JPEG"), "image/jpeg");
    assert_eq!(mime_from_path("x.gif"), "image/gif");
    assert_eq!(mime_from_path("x.webp"), "image/webp");
    assert_eq!(mime_from_path("x.bmp"), "image/png");
}
