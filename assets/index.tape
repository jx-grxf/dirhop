# Regenerate: cargo build --release && sh assets/demo-home.sh && vhs assets/index.tape
Output "/tmp/dirhop-index.gif"
Set Shell "bash"
Set FontSize 18
Set Width 1280
Set Height 720
Set Padding 24
Set Theme "Catppuccin Mocha"
Env HOME "/tmp/dirhop-demo"
Env DIRHOP_NO_UPDATE "1"
Hide
Type 'clear && ./target/release/dirhop index -q && sleep 61 && ./target/release/dirhop --index show'
Enter
Sleep 63s
Show
Sleep 500ms
Screenshot assets/index.png
Sleep 500ms
