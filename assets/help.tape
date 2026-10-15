# Regenerate: cargo build --release && sh assets/demo-home.sh && vhs assets/help.tape
Output "/tmp/dirhop-help.gif"
Set Shell "bash"
Set FontSize 18
Set Width 1280
Set Height 720
Set Padding 24
Set Theme "Catppuccin Mocha"
Env HOME "/tmp/dirhop-demo"
Env DIRHOP_NO_UPDATE "1"
Hide
Type 'clear && ./target/release/dirhop'
Enter
Sleep 1.2s
Show
Type "?"
Sleep 500ms
Screenshot assets/help.png
Sleep 300ms
