# The first scan (docs/03-functional-spec.md J0): `kipple` with no config file opens the
# onboarding, then the home screen. The `tapes` CI job runs it against demo-home with
# KIPPLE_TEST_ROOTS set and KIPPLE_CONFIG unset, so the default config dir is an empty
# directory inside the disposable demo. Transcript: first-scan.transcript.txt.

Output target/tapes/first-scan.gif
Output target/tapes/first-scan.webm
Output target/tapes/first-scan.txt

Require kipple

Set Shell bash
Set FontSize 16
Set Width 1280
Set Height 760
Set TypingSpeed 60ms
Set WaitTimeout 30s

Type "kipple"
Sleep 400ms
Enter

# Step 1: each level with what it would offer here, counted live while the scan runs.
Wait+Screen /step 1 of 4/
Wait+Screen /scan finished/
Sleep 2s
Down
Sleep 1500ms
Up
Sleep 1s
Enter

# Step 2: the project roots kipple found.
Wait+Screen /Where are your projects/
Sleep 2s
Enter

# Step 3: delete or Trash. The answer is only stored in this version.
Wait+Screen /Delete or move to Trash/
Sleep 1s
Down
Sleep 2s
Enter

# Step 4: prevention tips, shown and never applied.
Wait+Screen /Prevention tips/
Sleep 2500ms
Enter

# Finishing writes the answers once and opens the home screen.
Wait+Screen /Safe to clean/
Wait+Screen /scanned in/
Sleep 3s
Type "q"
Sleep 500ms
