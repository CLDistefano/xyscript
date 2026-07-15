# uc-harry.pgm
# Uppercase all instances of "harry" in the current file
QUIET
TF; XD
WHILE 1
  SE /harry/
	IF NOT $ERROR
		PW;DW;UC;XD;NW
	ELSE
		BREAK
	EI
ENDWHILE
TF
PR Done (file not saved)
EXIT


nn=QUIET;TF;XD;WHILE 1;SE /harry/;IF NOT $ERROR;PW;DW;UC;DE;XD;ELSE;BREAK;EI;EW;TF;PR Done (file not saved)