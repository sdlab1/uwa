# docs/demo.tape — regenerate with `vhs docs/demo.tape`
#
# Requires: vhs (https://github.com/charmbracelet/vhs), Chromium, uwa on $PATH.
# Runs on a clean machine in ~20 seconds. Output: docs/demo.gif.

Output docs/demo.gif

Set FontSize 16
Set Width 1000
Set Height 620
Set Padding 20
Set Theme "Catppuccin Mocha"
Set Shell "bash"
Set TypingSpeed 40ms

# --- Scene 1: fresh shell ---
Hide
Type "cd $(mktemp -d) && clear"
Enter
Show

Type "uwa start"
Enter
Sleep 2s

# The daemon prints its launch lines; give the user a beat to read them.
Sleep 3s

# --- Scene 2: switch to the dashboard ---
# Open the browser as a separate window on top of the terminal.
Hide
Type "open http://127.0.0.1:8080/#/clients 2>/dev/null || xdg-open http://127.0.0.1:8080/#/clients"
Enter
Show
Sleep 4s

# --- Scene 3: back to the shell, show a real request ---
Hide
Type "clear"
Enter
Show

Type `curl -N http://127.0.0.1:8080/v1/chat/completions -H 'content-type: application/json' -d '{"model":"gpt-4o","stream":true,"messages":[{"role":"user","content":"Say hello in one short sentence."}]}'`
Enter
Sleep 8s

# --- Scene 4: outro ---
Hide
Type "clear"
Enter
Show

Type "echo 'uwa  ·  one binary  ·  one command  ·  no token capture'"
Enter
Sleep 3s