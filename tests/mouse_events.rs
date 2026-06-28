mod common;

use common::record;

#[test]
fn test_mouse_events_in_helper_tool() {
    let helper_bin = common::get_helper_bin_path();
    let tape = format!(
        r#"
Output out.gif
Set Width 800
Set Height 600
Set FontSize 20
Set Framerate 30
Set Shell {helper_bin} mouse
Sleep 200ms
Click 5 5
Sleep 100ms
RightClick 10 10
Sleep 100ms
MouseMove 1 1 3 1
Sleep 100ms
MouseDrag 15 15 17 15
Sleep 100ms
Press q
Sleep 200ms
"#
    );

    let rec = record(&tape);

    // Get the final frame index
    let final_frame_idx = rec.frames.len() - 1;
    let final_frame = rec
        .reconstruct(final_frame_idx)
        .expect("reconstruct final frame");
    let cols = final_frame.cols as usize;

    // Helper to get cell at (col, row)
    let get_cell = |c: usize, r: usize| -> &evp::CellSnap { &final_frame.cells[r * cols + c] };

    // Expected Colors from Theme Constants (support both normal and bright variants):
    // Red (Left Click): normal [215, 78, 111], bright [254, 95, 134]
    let is_red = |bg: [u8; 3]| bg == [215, 78, 111] || bg == [254, 95, 134];
    // Green (Right Click): normal [49, 187, 113], bright [0, 215, 135]
    let is_green = |bg: [u8; 3]| bg == [49, 187, 113] || bg == [0, 215, 135];
    // Purple/Magenta (Drag): normal [237, 97, 215], bright [255, 122, 234]
    let is_purple = |bg: [u8; 3]| bg == [237, 97, 215] || bg == [255, 122, 234];
    // Light Blue (Move truecolor): [173, 216, 230]
    let expected_light_blue = [173, 216, 230];

    // Assert Click at (5, 5) turns Red
    let cell_click = get_cell(5, 5);
    assert!(
        is_red(cell_click.bg),
        "expected cell (5,5) to be Red/Bright Red: {:?}",
        cell_click
    );

    // Assert RightClick at (10, 10) turns Green
    let cell_rclick = get_cell(10, 10);
    assert!(
        is_green(cell_rclick.bg),
        "expected cell (10,10) to be Green/Bright Green: {:?}",
        cell_rclick
    );

    // Assert MouseMove path (1, 1) to (3, 1) turns Light Blue
    for c in 1..=3 {
        let cell_move = get_cell(c, 1);
        assert_eq!(
            cell_move.bg, expected_light_blue,
            "expected cell ({},1) to be Light Blue: {:?}",
            c, cell_move
        );
    }

    // Assert MouseDrag starts at (15, 15) with Red (Click), then (16, 15) and (17, 15) with Purple (Drag)
    let cell_drag_start = get_cell(15, 15);
    assert!(
        is_red(cell_drag_start.bg),
        "expected cell (15,15) to be Red/Bright Red: {:?}",
        cell_drag_start
    );

    let cell_drag_mid = get_cell(16, 15);
    assert!(
        is_purple(cell_drag_mid.bg),
        "expected cell (16,15) to be Purple/Bright Purple: {:?}",
        cell_drag_mid
    );

    let cell_drag_end = get_cell(17, 15);
    assert!(
        is_purple(cell_drag_end.bg),
        "expected cell (17,15) to be Purple/Bright Purple: {:?}",
        cell_drag_end
    );

    // Render to SVG string and assert mouse rendering exists in SVG
    let svg_opts = evp::render_svg::SvgOptions::default();
    let svg_str =
        evp::render_svg::render_svg_to_string(&rec, &svg_opts).expect("render svg to string");

    assert!(
        svg_str.contains(
            "<animateTransform attributeName=\"transform\" type=\"translate\" calcMode=\"discrete\""
        ),
        "expected SVG to contain discrete translate animation for mouse pointer"
    );
    assert!(
        svg_str.contains("fill=\"#ff0000\" fill-opacity=\"0.5\""),
        "expected SVG to contain red clicking ripple circle"
    );
    assert!(
        svg_str.contains("fill=\"#ed61d7\" fill-opacity=\"0.5\""),
        "expected SVG to contain purple dragging ripple circle"
    );
    assert!(
        svg_str.contains("<path d=\"M0,0 L0,30 L8,22 L14,36 L18,34 L12,20 L20,20 Z\""),
        "expected SVG to contain the cursor vector path"
    );
}
