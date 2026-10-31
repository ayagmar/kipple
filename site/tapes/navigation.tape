# Moving around the home screen (docs/03-functional-spec.md J1, J2). The `tapes` CI job
# runs it against demo-home with KIPPLE_TEST_ROOTS set and writes navigation.toml into
# the disposable $TAPES directory, so `--config` skips the onboarding.
# Transcript: navigation.transcript.txt.

Output target/tapes/navigation.gif
Output target/tapes/navigation.webm
Output target/tapes/navigation.txt

Require kipple

Set Shell bash
Set FontSize 16
Set Width 1280
Set Height 760
Set TypingSpeed 60ms
Set WaitTimeout 30s

Hide
Type `cd "$TAPES" && clear`
Enter
Sleep 500ms
Show

Type "kipple --config navigation.toml"
Sleep 400ms
Enter
Wait+Screen /scanned in/
Sleep 2s

# Move through the findings; the detail pane follows the cursor.
Type@700ms "jjj"
Sleep 1s
Type "k"
Sleep 1500ms

# Fold and unfold the group under the cursor.
Type "h"
Sleep 1s
Type "l"
Sleep 1s

# One lens at a time: Agents, Dev, System, then all again.
Tab@1s 2
Wait+Screen /lens: Dev/
Sleep 1500ms
Tab@1s 2
Sleep 1s

# Filter by name or path.
Type "/"
Sleep 300ms
Type "venv"
Enter
Wait+Screen /filter: venv/
Sleep 2s
Escape
Sleep 1s

# Select the Safe bucket, which opens its review. Leaving the review cleans nothing.
Type "c"
Wait+Screen /Review: clean/
Sleep 2500ms
Escape
Sleep 1s

# The settings screen, then the help.
Type ","
Wait+Screen /Config file/
Sleep 2500ms
Escape
Sleep 1s
Type "?"
Wait+Screen /this help/
Sleep 2500ms
Type "?"
Sleep 1s

Type "q"
Sleep 500ms
