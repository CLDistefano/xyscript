# multicaret.xpl — a short tour of multi-caret editing.
#
# RUN this in XyScript (each step opens its own fresh tab with NE) and watch it happen in the
# document. Nothing here needs TRUST. The idea: the CARET* verbs make several carets, then ORDINARY
# editing verbs (INSERT, case changes, delete, …) act on every caret at once, as a single undo step.

# ---------------------------------------------------------------------------------------------------
# 1. CARETALL — put a caret at every match, then edit them all at once (rename "cat" -> "fox").
# ---------------------------------------------------------------------------------------------------
ne
insert "cat and dog and cat and bird and cat"
tf
caretall/w cat            # a selection-caret on each whole-word "cat"
pr Step 1: $CARETS carets on "cat"
insert "fox"              # typing replaces every selected "cat" at once
caretone                  # collapse back to a single caret
tf
pr   -> $LI

# ---------------------------------------------------------------------------------------------------
# 2. CARETSPLIT — one caret per line, then append to every line at once.
# ---------------------------------------------------------------------------------------------------
ne
insert "apple"
enter
insert "pear"
enter
insert "plum"
dfa                       # select the whole document (all three lines)
caretsplit                # a bare caret at the end of each line
insert " (fruit)"         # appended to every line
caretone
pr Step 2: each line now ends with " (fruit)".

# ---------------------------------------------------------------------------------------------------
# 3. The $M... mirror arrays — read every caret from a script.
# ---------------------------------------------------------------------------------------------------
ne
insert "one two one two one"
tf
caretall one              # a caret on each "one"
pr Step 3: $CARETS carets. Their selections are:
for i, piece in $MSE
	pr   caret $i: [$piece] starting at offset $($MDS[$i])
endfor
caretone

pr All done. One Undo takes a whole multi-caret edit back at once.
