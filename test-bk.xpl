local num str
set num = 0
set str = ""
bk debug
while $num <= 20
	set num += 1
	set str = $str . $MSEC . " "
	pr Pass: $num
	wait 500
	bk on
endwhile
msg Done
