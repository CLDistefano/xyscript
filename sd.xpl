# "Smart" DIR command
# Usage RUN sd.xpl file[,ext]
IF NOT $1
	DIR
	EXIT
EI
LOCAL dcm n fnm ext farray spec swt
SET dcm = "DIR"
SET swt = ""
SET fnm = $*
BK debug
IF INDEX($fnm, "/") == 0 OR INDEX($fnm, "-") == 0
	SET swt = $1
	SET dcm = "DIR" . $swt
	SET fnm = LSTRIP(REPLACE($fnm, $swet))
EI
SET ext = "*"
IF NOT Contains($fnm, ",") 
	SET fnm = $fnm . "*"
	GOTO :a
EI
SET fnm = REPLACE($fnm, " ", "")
SET farray = SPLIT($fnm, ",")
SET fnm = ""
SET n = 0
WHILE $n < Len($farray) - 1
	SET fnm = $fnm . $farray[$n] . "*"
	SET n += 1
ENDWHILE
IF Len($farray[$n]) > 0 SET ext = $farray[$n] . "*"
:a
SET spec = $fnm
SET spec = $spec.$ext
CLERR
$dcm $spec
IF NOT $ERROR
	PR Files matching $spec
	EXIT
EI
IF LEN($ext) > 1
	SET ext = TRIMRIGHT($ext, 2)
	SET ext = $ext . "*"
	GOTO :a
EI
IF LEN($fnm) > 2
	SET fnm = TRIMRIGHT($fnm, 2)
	IF SUBSTR($fnm, LEN($fnm) - 1, 1) != "*" SET fnm = $fnm . "*"
	GOTO :a
ELSE
	PR No matching files
EI
