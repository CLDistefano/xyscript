# loop10.xpl -- count to 10
# test of EXITALL / EX1

QUIET
SET count = 1
:loop
PR Count is $count
WAIT 1000
SET count = $count + 1
IF $count == 5 CALL out
IF $count <= 10 GOTO loop
PR Done.
EXIT

SUB out
  MSG We are going to bail out of all scripts...
  EXITALL
ENDSUB
