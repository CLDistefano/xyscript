# Open `color-names.lst`, then RUN this script to swatch each R G B value with its own colour.
tf
while not $FE
	el
	se/r /\"\d{1,3} \d{1,3} \d{1,3}\"/
	if not $error mda/b $(trimright(trimleft($SE, 1), 1))
	ld
ew
tf
pr Done.
