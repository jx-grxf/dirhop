# Regenerate: cargo build --release && sh assets/demo-home.sh && vhs assets/setup.tape
Output assets/setup.gif
Set Shell "bash"
Set FontSize 18
Set Width 1280
Set Height 720
Set Padding 24
Set Theme "Catppuccin Mocha"
Env HOME "/tmp/dirhop-demo"
Env SHELL "/bin/zsh"
Env DIRHOP_NO_UPDATE "1"
Hide
Type 'clear && ./target/release/dirhop setup'
Enter
Sleep 800ms
Show
Down
Sleep 300ms
Screenshot assets/settings.png
Sleep 300ms
