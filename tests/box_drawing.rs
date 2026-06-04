mod common;

use common::record;
use evp::{FrameStyle, RenderOptions, WindowBarStyle};
#[test]
fn test_box_drawing_alignment() {
    // A script setting up bash, disabling prompt elements, padding/margin, and window bar,
    // hiding the setup commands, then printing U+2580 (▀) and U+2584 (▄).
    let tape = r#"
Output /tmp/box_drawing_test.gif
Set Width 80
Set Height 40
Set FontSize 20
Set Padding 0
Set Margin 0
Set WindowBar None
Set Framerate 10
Set Shell bash --norc
Hide
Type "export PS1='' PROMPT_COMMAND=''"
Enter
Type "stty -echo"
Enter
Type "printf '\x1b[?25l'"
Enter
Type "clear && echo -n '▀▄'"
Enter
Sleep 100ms
Show
Sleep 500ms
"#;

    let rec = record(tape);
    assert!(!rec.frames.is_empty(), "no frames captured");

    // Reconstruct the last frame
    let frame = rec
        .reconstruct(rec.frames.len() - 1)
        .expect("reconstruct last frame");

    // Generate a temporary file path for the PNG frame
    let mut temp_png = std::env::temp_dir();
    temp_png.push(format!("box_drawing_frame_{}.png", std::process::id()));

    // Custom render options: no padding, no margin, no window bar, default font size
    let mut opts = RenderOptions::default();
    opts.frame_style = FrameStyle {
        canvas_width_px: None,
        canvas_height_px: None,
        padding_px: 0,
        margin_px: 0,
        margin_fill: [23, 23, 23],
        window_bar: WindowBarStyle::None,
        window_bar_size_px: 0,
        border_radius_px: 0,
    };

    evp::render_gif::render_png_frame(&frame, &opts, &temp_png).expect("render PNG frame");

    // Load the rendered PNG
    let img = lodepng::decode24_file(&temp_png).expect("decode rendered png");
    let width = img.width;
    let height = img.height;
    let pixels = img.buffer;

    // Cleanup the temporary file
    let _ = std::fs::remove_file(&temp_png);

    let cell_w = rec.cell_width_px as usize;
    let cell_h = rec.cell_height_px as usize;

    assert!(width >= 2 * cell_w, "width too small: {width}");
    assert!(height >= cell_h, "height too small: {height}");

    let get_pixel = |x: usize, y: usize| {
        let p = pixels[y * width + x];
        [p.r, p.g, p.b]
    };

    // Col 0 contains U+2580 (▀) Upper Half Block:
    // - The top of Col 0 should be foreground color (fully opaque/non-background)
    // - The bottom of Col 0 should be background color
    let top_col0_pixel = get_pixel(cell_w / 2, 2);
    let bottom_col0_pixel = get_pixel(cell_w / 2, cell_h - 2);

    // Col 1 contains U+2584 (▄) Lower Half Block:
    // - The top of Col 1 should be background color
    // - The bottom of Col 1 should be foreground color
    let top_col1_pixel = get_pixel(cell_w + cell_w / 2, 2);
    let bottom_col1_pixel = get_pixel(cell_w + cell_w / 2, cell_h - 2);

    let bg_color = frame.default_bg;

    // Assert that top_col0 is drawn (not equal to background color)
    assert_ne!(
        top_col0_pixel, bg_color,
        "upper half block (col 0): top pixel at y=2 should not be background color"
    );
    // Assert that bottom_col0 is background color (not drawn)
    assert_eq!(
        bottom_col0_pixel,
        bg_color,
        "upper half block (col 0): bottom pixel at y={} should be background color",
        cell_h - 2
    );

    // Assert that top_col1 is background color (not drawn)
    assert_eq!(
        top_col1_pixel, bg_color,
        "lower half block (col 1): top pixel at y=2 should be background color"
    );
    // Assert that bottom_col1 is drawn (not equal to background color)
    assert_ne!(
        bottom_col1_pixel,
        bg_color,
        "lower half block (col 1): bottom pixel at y={} should not be background color",
        cell_h - 2
    );
}
