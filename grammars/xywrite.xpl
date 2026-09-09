# xywrite.xpl — Syntax highlighting grammar for XyWrite-formatted files
# RUN grammars/xywrite.xpl
# then open a .xpl/.cfg/.lib file; HIGHLIGHT on, or HLAUTO on
# For detailed instructions, see User Manual Chapter 16

HLLANG xywrite .xy,.xy3,.xy4

HLRULE xycomment "(^|\s);\*;(\s.*)?$"
HLCOLOR xycomment 35 142 35

HLRULE xyprompt "«(PR|NT|IV)[^«»]+»"
HLCOLOR xyprompt 0 255 255

HLRULE xylabel "«LB[^«»]+»"
HLCOLOR xylabel 127 0 255

HLRULE xyfootnote "«FN"
HLCOLOR xyfootnote 92 51 23

HLRULE xymode "«MD[^«»]+»"
HLCOLOR xymode 238 0 238

HLRULE xyformat "«\w[^«»]+»"
HLCOLOR xyformat 255 222 173
