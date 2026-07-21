SUB savesess
	# DOC: Save session info. and, optionally, close all open tabs
	# DOC: Usage: SAVESESS [file] [C|S] (default file: saved_session.xpl, in directory with xyscript.exe; C: close tabs; S: close and save changes)
# DOC: To restore a session, RUN the saved_session file
	local bclose fnm n sess
	set bclose = 0
	if Upper($1) == "C" or Upper($2) == C set bclose = 1
	if Upper($1) == "S" or Upper($2) == S set bclose = 2
	if Len($1) > 1
		set fnm = $1
	else if Len($2) > 1
		set fnm = $2
	else
		set fnm = $XSDIR.$DC."saved_session.xpl"
	ei
	set sess = ""
	for n = 1 to $TC
		if not $D1
			if not $FP
				if not $fs
					if $bclose; ab; else; bufnext; ei
				else
					af
				ei
			ei		
			set sess = "OPEN/NV ".$FP.$NL."jmp ".$CP.$NL.$sess
			if $bclose; if $bclose > 1; st/nv; else; ab/v; ei; else; bufnext; ei
		else
			set sess = "DIR ".$DP.$NL.$sess
			if $bclose; ab; else; bufnext; ei
		ei
	ef
	wf $fnm $sess
	pr Session saved.
ENDSUB
	
