# startup.xpl -- XyScript's startup script.
#
# XyScript runs this file automatically at launch, from the folder the program
# itself lives in ($XSDIR). Because the working directory starts at $XSDIR, the
# plain relative paths below resolve against THIS folder -- so you can zip the whole
# folder, move it to another machine, unzip it, and everything still loads, the same
# on Windows, macOS, and Linux.

trust 0                             # TRUST 1 allows shelling to the operating system
loadkbd xyscript-internal.kbd       # Keyboard map
loadcfg xyscript-default.cfg        # Configuration (all settings at their defaults)
theme ./themes/foolscap.xpl         # Colour theme
loadar xyscript.ar                  # Auto-replace pairs
loadlib xyscript.lib                # User script library; command `LISTLIB` to view routines

# `user-variables.xpl` holds variables that YOU set, used by some library routines.
# run user-variables.xpl            # Disabled; copy `user-variables.template`, edit it,
                                    # save it as `user-variables.xpl`, then delete the "# " above.

# cd ~/Documents                    # Optional starting folder (default is the program folder)
