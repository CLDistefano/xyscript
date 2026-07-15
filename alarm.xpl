local note tyme
quiet
set tyme = $1
set note = $*
msg /T:1 Alarm set for $tyme
while 1
	if time("h:mm") == $tyme or time("hh:mm") == $tyme or \
		index(upper($note), time("h:mm PM")) == 0
		MESSAGE $note
		break
	else
		wait 5000
	endif
endwhile
