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

# A mode span «MD…»…«MDNM» on ONE line: the two codes keep the xymode colour, and the text
# BETWEEN them is recoloured via the NAMED capture group `xytext` (see HLRULE in the reference —
# a group's name is the token kind for its span; the rest of the match keeps the rule's kind).
# This pair rule must precede the single-code rule below so it wins (first-match-wins).
HLRULE xymode "«MD[^«»]+»(?P<xytext>[^«]*)«MDNM»"
# Any remaining single mode code (an unpaired toggle, or one whose partner is on another line).
HLRULE xymode "«MD[^«»]+»"
HLCOLOR xymode 238 0 238
HLCOLOR xytext 70 130 180

HLRULE xyformat "«\w[^«»]+»"
HLCOLOR xyformat 255 222 173
