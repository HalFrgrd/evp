# Cols Rows: demonstrate setting only cols and rows without width and height.
#
# Runs the evp helper tool in grid mode to draw unit digits.

Output cols_rows.gif
Output cols_rows.svg

Set Shell ./target/x86_64-unknown-linux-musl/release/evp_helper_tool grid
Set Cols 30
Set Rows 10
Set FontSize 20
Set Framerate 30
Set Padding 10
Set Margin 10

Sleep 1s
Type " "
Sleep 500ms
