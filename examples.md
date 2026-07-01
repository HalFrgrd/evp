# EVP Examples

This page showcases various `.tape` example scripts and their output animations rendered with EVP. All these examples are rebuilt on every push to the master branch by the CI pipeline.

---

## 1. Dynamic Window Title Bar
Shows how to configure macOS-like window decoration controls and update the title dynamically at the center of the bar using standard ANSI OSC sequences.

* **Tape**: [`examples/window_title.tape`](examples/window_title.tape)
* **Demo**:
  ![Dynamic Window Title Bar Demo](https://github.com/HalFrgrd/evp/releases/download/assets/window_title.gif)

```elixir
# Enable window bar styling
Set WindowBar Rings

# Set a dynamic terminal title via ANSI OSC sequence
Type "printf '\033]2;My Dynamic Terminal Title\007'"
Enter
Sleep 2s
```

---

## 2. Colors & Themes
Demonstrates EVP's support for theme presets and standard ANSI 256 colors.

* **Tape**: [`examples/colors.tape`](examples/colors.tape)
* **Demo**:
  ![Colors Demo](https://github.com/HalFrgrd/evp/releases/download/assets/colors.gif)

---

## 3. Mouse Support
Demonstrates full mouse interaction support including clicking, dragging, movement, and wheel scrolling.

* **Tape**: [`examples/mouse.tape`](examples/mouse.tape)
* **Demo**:
  ![Mouse Demo](https://github.com/HalFrgrd/evp/releases/download/assets/mouse.gif)

---

## 4. Keyboard Shortcuts
Shows how complex key combinations and modifiers are recorded and replayed.

* **Tape**: [`examples/keys.tape`](examples/keys.tape)
* **Demo**:
  ![Keys Demo](https://github.com/HalFrgrd/evp/releases/download/assets/keys.gif)

---

## 5. Custom Shells & TUI Tours
Shows how you can boot directly into full screen TUI applications (like `yazi` or `htop`) instead of a traditional shell.

* **Tape**: [`examples/shell-tour.tape`](examples/shell-tour.tape)
* **Demo**:
  ![Shell Tour Demo](https://github.com/HalFrgrd/evp/releases/download/assets/shell-tour.gif)

---

## 6. Layout Customization (Padding & Margin)
Demonstrates setting inner padding and outer margins with custom fill colors.

* **Tape**: [`examples/padding.tape`](examples/padding.tape) / [`examples/margin.tape`](examples/margin.tape)
* **Demo**:
  ![Padding Demo](https://github.com/HalFrgrd/evp/releases/download/assets/padding.gif)

---

## 7. Embedded Font Customization
Shows how embedded monospace and Nerd Fonts are loaded and rendered.

* **Tape**: [`examples/embedded-font-demo.tape`](examples/embedded-font-demo.tape)
* **Demo**:
  ![Embedded Fonts Demo](https://github.com/HalFrgrd/evp/releases/download/assets/embedded-font-demo.gif)
