mod common;

use common::record;
use evp::{FrameStyle, RenderOptions, WindowBarStyle};

#[test]
fn test_braille_character_sizing() {
    let tape = r#"
Output /tmp/braille_test.gif
Set Width 80
Set Height 40
Set FontSize 24
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
Type "clear && echo -n '⣿'"
Enter
Sleep 100ms
Show
Sleep 500ms
"#;

    let rec = record(tape);
    assert!(!rec.frames.is_empty(), "no frames captured");

    let frame = rec
        .reconstruct(rec.frames.len() - 1)
        .expect("reconstruct last frame");

    let mut temp_png = std::env::temp_dir();
    temp_png.push(format!("braille_test_frame_{}.png", std::process::id()));

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

    let img = lodepng::decode24_file(&temp_png).expect("decode rendered png");
    let width = img.width;
    let height = img.height;
    let pixels = img.buffer;

    let _ = std::fs::remove_file(&temp_png);

    let cell_w = rec.cell_width_px as usize;
    let cell_h = rec.cell_height_px as usize;

    assert!(width >= cell_w, "width too small: {width}");
    assert!(height >= cell_h, "height too small: {height}");

    let get_pixel = |x: usize, y: usize| {
        let p = pixels[y * width + x];
        [p.r, p.g, p.b]
    };

    let bg_color = frame.default_bg;

    // '⣿' (U+28FF) is a full 2x4 braille pattern with all 8 dots populated.
    // At full scale (font size 24), dots span vertically across both upper and lower halves.
    let mut non_bg_count_upper = 0;
    let mut non_bg_count_lower = 0;

    for y in 0..cell_h {
        for x in 0..cell_w {
            if get_pixel(x, y) != bg_color {
                if y < cell_h / 2 {
                    non_bg_count_upper += 1;
                } else {
                    non_bg_count_lower += 1;
                }
            }
        }
    }

    // Assert that the full braille block has substantial pixel coverage in both upper and lower halves of the cell.
    assert!(
        non_bg_count_upper >= 4,
        "upper half of braille cell should have drawn pixels, found {non_bg_count_upper}"
    );
    assert!(
        non_bg_count_lower >= 4,
        "lower half of braille cell should have drawn pixels, found {non_bg_count_lower}"
    );
}
