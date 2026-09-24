use super::*;

fn png(width: u32, height: u32, noise: bool) -> Vec<u8> {
    let mut seed = 0x2545_f491_u32;
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        if noise {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let [a, b, c, _] = seed.to_le_bytes();
            image::Rgb([a, b, c])
        } else {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        }
    });
    let mut data = std::io::Cursor::new(vec![]);
    DynamicImage::ImageRgb8(image)
        .write_to(&mut data, ImageFormat::Png)
        .unwrap();
    data.into_inner()
}

#[test]
fn budget_and_sizes_follow_node() {
    // base64 200 000 字符对应 25 000 token：恰好在预算内。
    assert!(fits(150_000));
    assert!(!fits(150_003));
    assert_eq!(byte_count(512), "512B");
    assert_eq!(byte_count(1536), "1.5KB");
    assert_eq!(byte_count(20 * 1024 * 1024), "20MB");
    assert_eq!(byte_count(25_690_112), "24.5MB");
    assert_eq!(kind(Path::new("/a/B.PNG")), Some(Media::Image("image/png")));
    assert_eq!(
        kind(Path::new("clip.mov")),
        Some(Media::Video("video/quicktime"))
    );
    assert_eq!(kind(Path::new("notes.txt")), None);
}

#[test]
fn small_images_pass_through_and_large_ones_fit_the_budget() {
    let small = png(40, 30, false);
    let prepared = prepare(small.clone(), "image/png").unwrap();
    assert_eq!(prepared.data, small);
    assert_eq!(prepared.info["resized"], false);
    assert_eq!(prepared.info["dimensions"]["displayWidth"], 40);

    let wide = prepare(png(2400, 60, false), "image/png").unwrap();
    assert!(fits(wide.data.len()));
    assert_eq!(wide.info["resized"], true);
    assert_eq!(wide.info["dimensions"]["displayWidth"], 2000);

    // 噪声 PNG 无损过大：单向转 JPEG。
    let noisy = prepare(png(600, 600, true), "image/png").unwrap();
    assert_eq!(noisy.mime, "image/jpeg");
    assert!(fits(noisy.data.len()));
    assert_eq!(noisy.info["compressed"], true);
}

#[test]
fn failures_use_node_texts() {
    assert_eq!(
        prepare(vec![], "image/png").err().unwrap(),
        "Image file is empty (0 bytes)"
    );
    assert_eq!(
        prepare(b"not an image".to_vec(), "image/png")
            .err()
            .unwrap(),
        "Unable to decode image data"
    );
    let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
    webp.resize(200_000, 0);
    assert_eq!(
        prepare(webp, "image/webp").err().unwrap(),
        "WebP image exceeds the model image budget and the current image adapter cannot transcode WebP"
    );
}
