# startup.qns -- runs at launch (design 9.4: startup -> load -> operative keymap).
#
# Pass this file to the binary:  xyscript scripts/startup.qns
#
# It loads the sample layout. LOADKBD is transactional: if the file is missing
# or malformed the load is rejected, $ERROR is set, and the embedded default
# keymap stays operative -- so a broken layout never strands you.

# The binary already compiles in the demo layout, so this script just greets.
# To demonstrate LOADKBD from a script, point it at a layout on disk (the path
# is relative to the directory you launched from):
#   LOADKBD xyscript-kbd/kbd/xyscript-internal.kbd
PR startup.qns ran -- welcome to XyScript
