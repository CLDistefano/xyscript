# kbddoc.xpl — the body of the built-in KBDDOC command (embedded via include_str! in vm.rs).
# Adds a key-name documentation column to a KBD file, using GETKEY() to name each scan code.
#   KBDDOC            annotate the KBD file in the current tab
#   KBDDOC <file>     annotate <file> (opened, or blind-edited + saved if not already open)
local blind fnm name width comment knm
set blind = 0
set fnm = ""
if $1
	set fnm = $1
	if not exists($fnm); pr $fnm does not exist.; ex; ei
	gof $fnm
	if $error; bedit $fnm; set blind = 1; ei
ei
se/t /$NLKEYS=/
if $error
	pr Not a valid KBD file!
	if $blind; bclose $fnm; exitall; ei
	ex
ei
tf; se/t /$NLunmod Esc/
if not $error
	yd; tf
	pr KBD file already documented.
	if $blind bclose $fnm
	ex
ei
:a
sea /TABLE=/
if not $error
	lb
	set name = lower(trimleft($li, 6))
	if not $name set name = "unmod"
	set width = len($name) + 1 + len(" Backspace")
	set comment = ";Comment"
	while len($comment) < $width; set comment=$comment." "; ew
	:b
	ld; lb
	if index($LI, ";") == 0
		if len($LI) > 1 wr $comment
		goto :b
	ei
	set n = trimright($LI, len($LI) - index($li, "="))
	if not isdec($n) goto :a
	set knm = $name." ".$(getkey($n))." "
	while len($knm) < $width; set knm=$knm." ";ew
	set knm = $knm.":"
	wr $knm
	if $n < 104 goto :b
goto :a
ei
tf
if $blind; sa; bclose $fnm; ei
pr Done.
