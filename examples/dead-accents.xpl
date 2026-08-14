# Dead-accent insert for selected characters
# Usage: run dead-accents.xpl
# Press ' ` , ~ " or ^, then the letter to be accented (case-sensitive)
quiet
local trig char
rk trig Press ' ` , ~ " or ^
if not contains ("'`,~\"^", $trig) exit
switch $trig
	case "'" # Acute
		put ';RK char Acute: a A e E i I o O u U y Y;switch $char;case "a";BD;put á;case "A";BD;put Á;case "e";BD;put é;case "E";BD;put É;case "i";BD;put í;case "I";BD;put Í;case "o";BD;put ó;case "O";BD;put Ó;case "u";BD;put ú;case "U";BD;put Ú;case "y";BD;put ý;case "Y";BD;put Ý;default;put $char;ESW
	case "`" # Grave
		put `;RK char Grave: a A e E i I o O u U y Y;switch $char;case "a";BD;put à;case "A";BD;put À;case "e";BD;put è;case "E";BD;put È;case "i";BD;put ì;case "I";BD;put Ì;case "o";BD;put ò;case "O";BD;put Ò;case "u";BD;put ù;case "U";BD;put Ù;case "y";BD;put ỳ;case "Y";BD;put Ỳ;default;put $char;ESW
	case "," # Cedilla
		put ,;RK char Cedilla c C e E G s S;switch $char;case "c";BD;put ç;case "C";BD;put Ç;case "e";BD;put ȩ;case "E";BD;put Ȩ;case "G";BD;put Ģ;case "s";BD;put ş;case "S";BD;put Ş;default;put $char;ESW
	case "~" # Tilde
		put ~;RK char Tilde: a A n N o O;switch $char;case "a";BD;put ã;case "A";BD;put Ã;case "n";BD;put ñ;case "N";BD;put Ñ;case "o";BD;put õ;case "O";BD;put Õ;default;put $char;ESW
	case "\"" # Umlaut
		put \";RK char Umlaut: a A e E i I o O u U y Y;switch $char;case "a";BD;put ä;case "A";BD;put Ä;case "e";BD;put ë;case "E";BD;put Ë;case "i";BD;put ï;case "I";BD;put Ï;case "o";BD;put ö;case "O";BD;put Ö;case "u";BD;put ü;case "U";BD;put Ü;case "y";BD;put ÿ;case "Y";BD;put Ÿ;default;put $char;ESW
	case "^" # Circumflex
		put ^;RK char Circumflex: a A e E i I o O u U y Y;switch $char;case "a";BD;put â;case "A";BD;put Â;case "e";BD;put ê;case "E";BD;put Ê;case "i";BD;put î;case "I";BD;put Î;case "o";BD;put ô;case "O";BD;put Ô;case "u";BD;put û;case "U";BD;put Û;case "y";BD;put ŷ;case "Y";BD;put Ŷ;default;put $char;ESW;CLEAR char
esw
