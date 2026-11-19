#!/bin/sh
# A guided tour of Asqu on a tiny pretend website. Takes about 5 minutes.
#
#   sh demo.sh
#
# Creates ~/asqu-demo (only ever deletes it if it was made by this demo), walks through
# saving, undo, going back in plain words, milestones and Tries, then opens the Asqu view.
# Press Enter to move to each next step.
#
# Options: ASQU_DEMO_DIR=/somewhere  ASQU_DEMO_FAST=1 (no pauses)  ASQU_DEMO_NO_OPEN=1
set -eu

DEMO="${ASQU_DEMO_DIR:-$HOME/asqu-demo}"
HERE="$(cd "$(dirname "$0")" && pwd)"
if command -v asqu >/dev/null 2>&1; then ASQU="$(command -v asqu)"
elif [ -x "$HERE/target/release/asqu" ]; then ASQU="$HERE/target/release/asqu"
else echo "Asqu isn't installed. Build it first: cargo build --release" >&2; exit 1; fi

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
say() { printf '%s\n' "$*"; }
pause() { [ "${ASQU_DEMO_FAST:-}" = 1 ] || { printf '\n\033[2m[Enter to continue]\033[0m '; read -r _ </dev/tty; }; }
run() { printf '\n\033[36m$ %s\033[0m\n' "$*"; sh -c "$*" || true; }
asqu() { "$ASQU" "$@"; }
# Pretend an AI made a change, the way `asqu setup` asks any AI to: say the request,
# edit, then save. (`asqu run` tags the edit with the tool's name.)
ai() {
  why="$1"; shift
  "$ASQU" intent "$why" >/dev/null
  "$ASQU" run --name "Claude Code" -- sh -c "$*" 2>/dev/null
  "$ASQU" save >/dev/null
}

# ---------------------------------------------------------------------------
if [ -e "$DEMO" ]; then
  [ -f "$DEMO/.asqu/made-by-demo" ] || { echo "$DEMO already exists and wasn't made by this demo. Set ASQU_DEMO_DIR to use another folder." >&2; exit 1; }
  rm -rf "$DEMO"
fi
mkdir -p "$DEMO/.asqu" && cd "$DEMO" && touch .asqu/made-by-demo
export PATH="$(dirname "$ASQU"):$PATH"
alias_note="(using $ASQU)"

bold "Asqu demo: a tiny shop website $alias_note"
say "We'll build a small website, let a pretend AI change it, and use Asqu to see,"
say "undo and try things. Every command is shown in blue so you can run it yourself later."
cat > index.html <<'EOF'
<h1>Bean & Leaf</h1>
<a href="signup.html">Join us</a>
EOF
cat > signup.html <<'EOF'
<h2>Sign up</h2>
<p>Step 1: Your name</p>
<p>Step 2: Your email</p>
<p>Step 3: Your address</p>
<p>Step 4: Your favourite coffee</p>
EOF
printf 'body { color: black; }\n' > style.css
pause

bold "1. Saving: no setup, no 'init'"
say "The first Asqu command in a folder creates its history silently. Save by hand like this,"
say "or let it happen automatically (asqu watch, your AI tool's hooks via asqu setup, or asqu run)."
run "asqu save -w 'Start the shop website'"
pause

bold "2. An AI makes a change, and Asqu records WHY"
say "A pretend AI shortens the signup form. It tells Asqu the request first, then edits."
ai "Make signup faster" "printf '<h2>Sign up</h2>\n<p>Step 1: Your email</p>\n<p>Step 2: Pick a password</p>\n' > signup.html"
run "asqu log"
say "Each line shows what changed (Asqu writes that itself), the request behind it, and who did it."
pause

say ""
say "Any AI can do this, not just Claude: \`asqu setup\` adds a short note to AGENTS.md (the"
say "instruction file most AI tools read) asking it to run \`asqu intent \"<your request>\"\` first."
pause

bold "3. Look inside one change"
run "asqu show 2"
pause

bold "4. Mark a milestone"
say "Name a point you might want to come back to."
run "asqu mark 'v1 launched'"
pause

bold "5. Oops: the AI breaks something"
ai "Tidy up the homepage" "rm index.html; printf 'body { color: red; background: yellow; }\n' > style.css"
run "asqu log -n 2"
say "The homepage is gone. First, preview what undo would do (nothing changes yet):"
run "asqu undo --dry-run"
pause
run "asqu undo"
run "ls"
say "index.html is back. The undo itself is saved too, so even an undo can be undone."
pause

bold "6. Go back in plain words"
say "You can say 'Tuesday', '2 hours ago', '#3', a milestone, or 'before <something>':"
run "asqu back before the signup change --dry-run"
say "(Just a preview. Asqu matched your words against what was asked and what changed.)"
run "asqu back v1 launched --dry-run"
say "(Nothing to change: the undo already brought us back to the v1 milestone.)"
pause

bold "7. Tries: try ideas safely, side by side"
say "A Try is a separate copy of the project. The real project isn't touched until you Keep one."
run "asqu try start blue -w 'Make the site blue'"
run "asqu try start green -w 'Make the site green'"
printf 'body { color: navy; }\n' > .asqu/tries/blue/style.css
printf 'body { color: darkgreen; }\n' > .asqu/tries/green/style.css
run "asqu try list"
run "cat style.css"
say "Still black: the Tries are separate. Keep the blue one, toss the green one:"
pause
run "asqu try keep blue"
run "asqu try toss green"
run "cat style.css"
pause

bold "8. The risk rule: Asqu asks before risky changes"
run "asqu try start checkout -w 'Add checkout and remove the old page'"
rm .asqu/tries/checkout/signup.html
printf 'function charge(card) {}\n' > .asqu/tries/checkout/payments.js
run "asqu try keep checkout"
say "Removing a file and touching payments counts as high risk, so it wasn't kept."
say "A person can approve it (asqu try keep checkout --yes, or a button in the view)."
say "An AI can never approve it on your behalf."
pause

bold "9. Today in one sentence, and a tamper check"
run "asqu summary"
run "asqu verify"
pause

if [ "${ASQU_DEMO_NO_OPEN:-}" = 1 ]; then
  bold "Done. Open the view any time with:  cd $DEMO && asqu open"
  exit 0
fi
bold "10. The Asqu view"
say "Opening the simple view in your browser. Try 'Undo last change', click a change to see"
say "its files, and press Keep on the 'checkout' Try to see the question it asks."
say "Press Ctrl+C here when you're finished."
cd "$DEMO" && exec "$ASQU" open
