# Regenerate: cargo build --release && sh assets/demo-home.sh && vhs assets/demo.tape
Output assets/demo.gif
Set Shell "bash"
Set FontSize 18
Set Width 1280
Set Height 720
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 90ms
Env HOME "/tmp/dirhop-demo"
Env DIRHOP_NO_UPDATE "1"
Env PS1 "\[\e[1;36m\]\w\[\e[0m\] ❯ "
Hide
Type 'eval "$(./target/release/dirhop init bash)" && ./target/release/dirhop index -q && cd ~ && clear'
Enter
Sleep 300ms
Show
Sleep 600ms
Type "hop"
Sleep 400ms
Enter
Sleep 1.6s
Screenshot assets/recent.png
Type "wea"
Sleep 1.4s
Screenshot assets/screenshot.png
Sleep 600ms
Down
Sleep 900ms
Up
Sleep 700ms
Enter
Sleep 700ms
Type "ls"
Enter
Sleep 1.2s
Type "hop dirhop"
Sleep 300ms
Enter
Sleep 1.2s
Type "hop -"
Sleep 300ms
Enter
Sleep 2s
