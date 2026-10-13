use stet_tiny_skia::*;

#[test]
fn clone_rect_1() {
    let mut pixmap = Pixmap::new(200, 200).unwrap();

    let mut paint = Paint::default();
    paint.set_color_rgba8(50, 127, 150, 200);
    paint.anti_alias = true;

    pixmap.fill_path(
        &PathBuilder::from_circle(100.0, 100.0, 80.0).unwrap(),
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    let part = pixmap.as_ref().clone_rect(IntRect::from_xywh(10, 15, 80, 90).unwrap()).unwrap();

    let expected = Pixmap::load_png("tests/images/pixmap/clone-rect-1.png").unwrap();
    assert_eq!(part, expected);
}

#[test]
fn clone_rect_2() {
    let mut pixmap = Pixmap::new(200, 200).unwrap();

    let mut paint = Paint::default();
    paint.set_color_rgba8(50, 127, 150, 200);
    paint.anti_alias = true;

    pixmap.fill_path(
        &PathBuilder::from_circle(100.0, 100.0, 80.0).unwrap(),
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    let part = pixmap.as_ref().clone_rect(IntRect::from_xywh(130, 120, 80, 90).unwrap()).unwrap();

    let expected = Pixmap::load_png("tests/images/pixmap/clone-rect-2.png").unwrap();
    assert_eq!(part, expected);
}

#[test]
fn clone_rect_out_of_bound() {
    let mut pixmap = Pixmap::new(200, 200).unwrap();

    let mut paint = Paint::default();
    paint.set_color_rgba8(50, 127, 150, 200);
    paint.anti_alias = true;

    pixmap.fill_path(
        &PathBuilder::from_circle(100.0, 100.0, 80.0).unwrap(),
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    assert!(pixmap.as_ref().clone_rect(IntRect::from_xywh(250, 15, 80, 90).unwrap()).is_none());
    assert!(pixmap.as_ref().clone_rect(IntRect::from_xywh(10, 250, 80, 90).unwrap()).is_none());
    assert!(pixmap.as_ref().clone_rect(IntRect::from_xywh(10, -250, 80, 90).unwrap()).is_none());
}

#[test]
fn fill() {
    let c = Color::from_rgba8(50, 100, 150, 200);
    let mut pixmap = Pixmap::new(10, 10).unwrap();
    pixmap.fill(c);
    assert_eq!(pixmap.pixel(1, 1).unwrap(), c.premultiply().to_color_u8());
}

#[test]
fn draw_pixmap() {
    // Tests that painting algorithm will switch `Bicubic`/`Bilinear` to `Nearest`.
    // Otherwise we will get a blurry image.

    // A pixmap with the bottom half filled with solid color.
    let sub_pixmap = {
        let mut paint = Paint::default();
        paint.set_color_rgba8(50, 127, 150, 200);
        paint.anti_alias = false;

        let rect = Rect::from_xywh(0.0, 50.0, 100.0, 50.0).unwrap();

        let mut pixmap = Pixmap::new(100, 100).unwrap();
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        pixmap
    };

    let mut paint = PixmapPaint::default();
    paint.quality = FilterQuality::Bicubic;

    let mut pixmap = Pixmap::new(200, 200).unwrap();
    pixmap.draw_pixmap(20, 20, sub_pixmap.as_ref(), &paint, Transform::identity(), None);

    let expected = Pixmap::load_png("tests/images/canvas/draw-pixmap.png").unwrap();
    assert_eq!(pixmap, expected);
}

#[test]
fn draw_pixmap_ts() {
    let triangle = {
        let mut paint = Paint::default();
        paint.set_color_rgba8(50, 127, 150, 200);
        paint.anti_alias = true;

        let mut pb = PathBuilder::new();
        pb.move_to(0.0, 100.0);
        pb.line_to(100.0, 100.0);
        pb.line_to(50.0, 0.0);
        pb.close();
        let path = pb.finish().unwrap();

        let mut pixmap = Pixmap::new(100, 100).unwrap();
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        pixmap
    };

    let mut paint = PixmapPaint::default();
    paint.quality = FilterQuality::Bicubic;

    let mut pixmap = Pixmap::new(200, 200).unwrap();
    pixmap.draw_pixmap(
        5, 10,
        triangle.as_ref(),
        &paint,
        Transform::from_row(1.2, 0.5, 0.5, 1.2, 0.0, 0.0),
        None,
    );

    let expected = Pixmap::load_png("tests/images/canvas/draw-pixmap-ts.png").unwrap();
    assert_eq!(pixmap, expected);
}

#[test]
fn draw_pixmap_opacity() {
    let triangle = {
        let mut paint = Paint::default();
        paint.set_color_rgba8(50, 127, 150, 200);
        paint.anti_alias = true;

        let mut pb = PathBuilder::new();
        pb.move_to(0.0, 100.0);
        pb.line_to(100.0, 100.0);
        pb.line_to(50.0, 0.0);
        pb.close();
        let path = pb.finish().unwrap();

        let mut pixmap = Pixmap::new(100, 100).unwrap();
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        pixmap
    };

    let mut paint = PixmapPaint::default();
    paint.quality = FilterQuality::Bicubic;
    paint.opacity = 0.5;

    let mut pixmap = Pixmap::new(200, 200).unwrap();
    pixmap.draw_pixmap(
        5, 10,
        triangle.as_ref(),
        &paint,
        Transform::from_row(1.2, 0.5, 0.5, 1.2, 0.0, 0.0),
        None,
    );

    let expected = Pixmap::load_png("tests/images/canvas/draw-pixmap-opacity.png").unwrap();
    assert_eq!(pixmap, expected);
}

/// `draw_pixmap` as it was before it passed over transparent rows and
/// copied opaque ones: the pixmap as a pattern, through the pipeline.
fn draw_pixmap_through_pipeline(
    onto: &mut Pixmap,
    x: i32,
    y: i32,
    pixmap: PixmapRef,
    opacity: f32,
    mask: Option<&Mask>,
) {
    let rect = Rect::from_xywh(x as f32, y as f32, pixmap.width() as f32, pixmap.height() as f32)
        .unwrap();
    let paint = Paint {
        shader: Pattern::new(
            pixmap,
            SpreadMode::Pad,
            FilterQuality::Nearest,
            opacity,
            Transform::from_translate(x as f32, y as f32),
        ),
        blend_mode: BlendMode::SourceOver,
        anti_alias: false,
        force_hq_pipeline: false,
    };
    onto.fill_rect(rect, &paint, Transform::identity(), mask);
}

/// A pixmap drawn over another gives the pipeline's pixels, whichever rows
/// are passed over, copied or blended.
#[test]
fn draw_pixmap_matches_the_pipeline_row_by_row() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 32) as u8
    };
    const W: u32 = 300;
    const H: u32 = 64;

    // Rows by kind: transparent, opaque in every value a channel can
    // take, part transparent, and translucent.
    let mut source = Pixmap::new(W, H).unwrap();
    for (row, pixels) in source.data_mut().chunks_exact_mut(W as usize * 4).enumerate() {
        for (col, px) in pixels.chunks_exact_mut(4).enumerate() {
            let alpha = match row % 8 {
                0 | 1 => 0,
                2 | 3 | 4 => 255,
                5 => [0, 255][col / 40 % 2],
                _ => next(),
            };
            let channel = |value: u8| (u16::from(value) * u16::from(alpha) / 255) as u8;
            let rgb = if row % 8 == 2 {
                [col as u8, (col as u8).wrapping_add(85), 255 - col as u8]
            } else {
                [next(), next(), next()]
            };
            px.copy_from_slice(&[channel(rgb[0]), channel(rgb[1]), channel(rgb[2]), alpha]);
        }
    }

    let mut backdrop = Pixmap::new(260, 90).unwrap();
    for px in backdrop.data_mut().chunks_exact_mut(4) {
        let alpha = [255, 255, next(), 0][usize::from(next() % 4)];
        let channel = |value: u8| (u16::from(value) * u16::from(alpha) / 255) as u8;
        px.copy_from_slice(&[channel(next()), channel(next()), channel(next()), alpha]);
    }

    // Masks: rows wholly open, wholly shut, and mixed.
    let mut mask = Mask::new(260, 90).unwrap();
    for (row, values) in mask.data_mut().chunks_exact_mut(260).enumerate() {
        for value in values.iter_mut() {
            *value = match row % 5 {
                0 | 1 => 255,
                2 => 0,
                _ => next(),
            };
        }
    }

    for (x, y) in [(0, 0), (-37, -9), (11, 30), (-290, 5), (255, -60), (0, 41), (203, 0), (400, 400)] {
        for opacity in [1.0, 0.5] {
            for mask in [None, Some(&mask)] {
                let mut got = backdrop.clone();
                let mut want = backdrop.clone();
                let paint = PixmapPaint {
                    opacity,
                    blend_mode: BlendMode::SourceOver,
                    quality: FilterQuality::Nearest,
                };
                got.draw_pixmap(x, y, source.as_ref(), &paint, Transform::identity(), mask);
                draw_pixmap_through_pipeline(&mut want, x, y, source.as_ref(), opacity, mask);
                assert!(
                    got == want,
                    "at {x},{y}, opacity {opacity}, mask {}",
                    mask.is_some()
                );
            }
        }
    }
}
