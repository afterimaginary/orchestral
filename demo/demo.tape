# A real run, recorded with VHS (https://github.com/charmbracelet/vhs):
# a worker takes an issue in a throwaway GitHub repository to a reviewed PR.
#
#   ORCHESTRAL_SANDBOX=<clone> ORCHESTRAL_BIN=<dir with orchestral> vhs demo/demo.tape
#
# Paths under $HOME are shown as ~. The waits are hidden, so the GIF skips the minutes the worker spends working.
# Afterwards, close the PR and issue and run `orchestral cleanup` in the clone.

Output docs/demo.gif

Set Shell bash
Set FontSize 15
Set Width 1100
Set Height 680
Set Padding 18
Set TypingSpeed 35ms
Set Theme "Catppuccin Mocha"

Hide
Type `cd "$ORCHESTRAL_SANDBOX" && export PATH="$ORCHESTRAL_BIN:$PATH" TZ=UTC PS1='\[\e[1;35m\]sandbox\[\e[0m\] $ ' GH_PAGER= NO_COLOR= && orchestral() { command orchestral "$@" 2>&1 | sed "s|$HOME|~|g"; } && clear`
Enter
Sleep 1s
Show

Type "# One issue, one worker. The orchestrator session usually runs these for you."
Enter
Sleep 1s
Type `gh issue create --title "Add slugify() to textkit" --body "Add slugify(text) to textkit.py: lowercase, words joined by dashes, punctuation dropped. With tests."`
Enter
Wait+Screen@60s /issues\/\d+/
Sleep 1.5s
Type `ISSUE=$(gh issue list --limit 1 --json number -q '.[0].number')`
Enter
Sleep 500ms
Type `orchestral spawn slugify-text --issue $ISSUE --prompt "Implement #$ISSUE."`
Enter
Wait@90s /sandbox \$$/
Sleep 2s
Type "orchestral status"
Enter
Wait@90s /sandbox \$$/
Sleep 4s

Hide
Type `until [ "$(gh pr list --head slugify-text --json isDraft -q '.[0].isDraft')" = false ]; do sleep 15; done; sleep 20; clear`
Enter
Wait@40m /sandbox \$$/
Show

Type "# Later: the worker opened a PR, had it reviewed, and marked it ready."
Enter
Sleep 1s
Type "orchestral status"
Enter
Wait@90s /sandbox \$$/
Sleep 4s
Type "gh pr list --head slugify-text"
Enter
Wait@90s /sandbox \$$/
Sleep 2s
Type "orchestral merge-check $(gh pr list --head slugify-text --json number -q '.[0].number')"
Enter
Wait@90s /sandbox \$$/
Sleep 6s
