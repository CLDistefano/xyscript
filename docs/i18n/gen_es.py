#!/usr/bin/env python3
"""Generate a sample SPANISH catalog from the English template (structure stays in sync)."""
import os, re, sys
SRC, DST = sys.argv[1], sys.argv[2]

ES = {
    # menu bar
    "menubar.file": "Archivo", "menubar.edit": "Edición", "menubar.search": "Buscar",
    "menubar.view": "Ver", "menubar.document": "Documento", "menubar.tools": "Herramientas",
    "menubar.settings": "Configuración", "menubar.help": "Ayuda",
    # File
    "menu.file.new": "Nuevo", "menu.file.open": "Abrir…",
    "menu.file.open_readonly": "Abrir como solo lectura…", "menu.file.revert": "Revertir a la versión guardada",
    "menu.file.save": "Guardar", "menu.file.save_as": "Guardar como…",
    "menu.file.save_selection_as": "Guardar selección como…", "menu.file.save_all": "Guardar todo",
    "menu.file.export_pdf": "Exportar a PDF…", "menu.file.close": "Cerrar", "menu.file.quit": "Salir",
    # Edit
    "menu.edit.undo": "Deshacer", "menu.edit.redo": "Rehacer", "menu.edit.cut": "Cortar",
    "menu.edit.copy": "Copiar", "menu.edit.append_clipboard": "Anexar al portapapeles",
    "menu.edit.prepend_clipboard": "Anteponer al portapapeles", "menu.edit.paste": "Pegar",
    "menu.edit.select_all": "Seleccionar todo", "menu.edit.delete_selection": "Eliminar selección",
    "menu.edit.copy_to_cursor": "Copiar al cursor", "menu.edit.move_to_cursor": "Mover al cursor",
    "menu.edit.case": "Cambiar mayúsculas y minúsculas", "menu.edit.case.upper": "MAYÚSCULAS",
    "menu.edit.case.lower": "minúsculas", "menu.edit.case.swap": "Invertir mayúsculas y minúsculas",
    "menu.edit.text_colour": "Color del texto…", "menu.edit.clear_text_colour": "Quitar color del texto",
    "menu.edit.insert_file": "Insertar archivo…",
    # Search
    "menu.search.find": "Buscar…", "menu.search.replace": "Reemplazar…",
    "menu.search.find_next": "Buscar siguiente", "menu.search.find_prev": "Buscar anterior",
    "menu.search.grep": "Grep…", "menu.search.emphasize": "Resaltar",
    "menu.search.emphasize.selection": "Selección", "menu.search.emphasize.word": "Palabra o frase…",
    "menu.search.emphasize.regex": "Expresión regular…", "menu.search.emphasize.clear": "Borrar",
    # View
    "menu.view.highlighting": "Resaltado de sintaxis", "menu.view.paragraph_marks": "Marcas de párrafo",
    "menu.view.whitespace": "Espacios en blanco", "menu.view.line_numbers": "Números de línea",
    "menu.view.word_wrap": "Ajuste de línea", "menu.view.wrap_hang": "Sangría francesa ({n})…",
    "menu.view.preview_pane": "Panel de vista previa", "menu.view.shell_pane": "Panel de shell",
    "menu.view.shell_output": "Pestaña de salida de shell", "menu.view.split": "Dividir paneles",
    "menu.view.split.side_by_side": "Dividir en paralelo", "menu.view.split.top_bottom": "Dividir arriba y abajo",
    "menu.view.split.focus_next": "Activar el panel siguiente", "menu.view.split.swap": "Intercambiar los dos paneles",
    "menu.view.split.zoom": "Ampliar el panel activo (alternar)", "menu.view.split.fullscreen": "Panel activo en pantalla completa",
    "menu.view.split.remove": "Quitar división", "menu.view.menu_bar": "Barra de menús",
    "menu.view.title_bar": "Barra de título", "menu.view.writers_view": "Vista de escritor (Esc para salir)",
    "menu.view.cmdline": "Línea de comandos", "menu.view.cmdline.popup": "Ventana emergente",
    "menu.view.cmdline.position": "Posición", "menu.view.cmdline.position.top": "Arriba",
    "menu.view.cmdline.position.bottom": "Abajo", "menu.view.cmdline.position.hidden": "Oculta",
    # Document
    "menu.document.doctype": "Tipo de documento", "menu.document.doctype.typst": "Typst",
    "menu.document.doctype.gfm": "GFM (Markdown)", "menu.document.doctype.auto": "Automático (por extensión)",
    "menu.document.encoding": "Codificación de texto", "menu.document.encoding.display_as": "Mostrar como",
    "menu.document.encoding.convert_to": "Convertir a (archivo nuevo)", "menu.document.line_endings": "Fin de línea",
    "menu.document.line_endings.lf": "LF (Unix)", "menu.document.line_endings.crlf": "CRLF (Windows)",
    "menu.document.read_only": "Solo lectura", "menu.document.word_count": "Recuento de palabras",
    "menu.document.char_count": "Recuento de caracteres", "menu.document.properties": "Propiedades…",
    # Tools
    "menu.tools.run_script": "Ejecutar script…", "menu.tools.run_script_bg": "Ejecutar script en segundo plano…",
    "menu.tools.run_here": "Ejecutar el código bajo el cursor", "menu.tools.shell_command": "Comando de shell…",
    "menu.tools.jobs": "Tareas en segundo plano", "menu.tools.lang": "Corrección lingüística",
    "menu.tools.lang.check": "Revisar documento", "menu.tools.lang.spelling": "Subrayar ortografía",
    "menu.tools.lang.grammar": "Subrayar gramática", "menu.tools.lang.spell_next": "Siguiente error de ortografía",
    "menu.tools.lang.spell_prev": "Error de ortografía anterior", "menu.tools.lang.grammar_next": "Siguiente error de gramática",
    "menu.tools.lang.grammar_prev": "Error de gramática anterior", "menu.tools.lang.fix": "Corregir el problema en el cursor…",
    "menu.tools.lang.learn": "Aprender las palabras seleccionadas", "menu.tools.lang.dialect": "Variante lingüística",
    "menu.tools.lang.dialect.american": "Estadounidense", "menu.tools.lang.dialect.british": "Británico",
    "menu.tools.lang.dialect.canadian": "Canadiense", "menu.tools.lang.dialect.australian": "Australiano",
    "menu.tools.lang.dialect.indian": "Indio", "menu.tools.lang.spell_style": "Estilo de ortografía",
    "menu.tools.lang.grammar_style": "Estilo de gramática", "menu.tools.email": "Correo electrónico",
    "menu.tools.email.list": "Listar buzón", "menu.tools.email.save": "Guardar mensajes…",
    "menu.tools.email.delete": "Eliminar mensajes…", "menu.tools.email.send": "Enviar…",
    "menu.tools.email.attach": "Adjuntar archivo…", "menu.tools.email.clear_attach": "Quitar adjuntos",
    "menu.tools.email.verbose": "Registro detallado", "menu.tools.library": "Biblioteca",
    "menu.tools.library.load": "Cargar…", "menu.tools.library.reload": "Volver a cargar",
    "menu.tools.library.unload": "Descargar todo", "menu.tools.library.list": "Listar rutinas",
    "menu.tools.keyboard_chart": "Mapa del teclado",
    # Settings
    "menu.settings.appearance": "Apariencia", "menu.settings.appearance.colors": "Colores",
    "menu.settings.appearance.colors.editor_text": "Texto del editor…", "menu.settings.appearance.colors.editor_bg": "Fondo del editor…",
    "menu.settings.appearance.colors.selection_text": "Texto de la selección…", "menu.settings.appearance.colors.selection_bg": "Fondo de la selección…",
    "menu.settings.appearance.colors.caret": "Cursor…", "menu.settings.appearance.colors.separators": "Separadores…",
    "menu.settings.appearance.colors.tab_bg": "Fondo de la barra de pestañas", "menu.settings.appearance.colors.tab_bg.active": "Pestaña activa…",
    "menu.settings.appearance.colors.tab_bg.hover": "Pestaña bajo el puntero…", "menu.settings.appearance.colors.tab_bg.inactive": "Pestaña inactiva…",
    "menu.settings.appearance.colors.tab_close": "Cerrar pestaña ×…", "menu.settings.appearance.colors.ui_text": "Texto de la interfaz…",
    "menu.settings.appearance.colors.ui_bg": "Fondo de la interfaz…", "menu.settings.appearance.colors.txt_text": "Texto de la ventana TXT…",
    "menu.settings.appearance.colors.txt_bg": "Fondo de la ventana TXT…", "menu.settings.appearance.colors.dir_text": "Texto de la ventana de directorio…",
    "menu.settings.appearance.colors.dir_bg": "Fondo de la ventana de directorio…", "menu.settings.appearance.fonts": "Fuentes",
    "menu.settings.appearance.fonts.editor": "Editor…", "menu.settings.appearance.fonts.ui": "Interfaz…",
    "menu.settings.appearance.fonts.txt": "Ventana TXT…", "menu.settings.appearance.fonts.dir": "Ventana de directorio…",
    "menu.settings.appearance.fonts.preview_body": "Cuerpo de vista previa/PDF…", "menu.settings.appearance.fonts.preview_code": "Código de vista previa/PDF…",
    "menu.settings.appearance.ui_zoom": "Zoom de la interfaz", "menu.settings.appearance.ui_zoom.in": "Acercar",
    "menu.settings.appearance.ui_zoom.out": "Alejar", "menu.settings.appearance.ui_zoom.reset": "Restablecer (100 %)",
    "menu.settings.appearance.margins": "Márgenes", "menu.settings.appearance.margins.wider": "Más anchos",
    "menu.settings.appearance.margins.narrower": "Más estrechos", "menu.settings.appearance.margins.none": "Ninguno",
    "menu.settings.appearance.line_spacing": "Interlineado", "menu.settings.appearance.line_spacing.more": "Más",
    "menu.settings.appearance.line_spacing.less": "Menos", "menu.settings.appearance.reset_ui": "Restablecer el tamaño de la interfaz",
    "menu.settings.appearance.reset_dw": "Restablecer la ventana de directorio", "menu.settings.syntax": "Resaltado de sintaxis",
    "menu.settings.syntax.theme_colors": "Colores del tema", "menu.settings.syntax.token_kinds": "Tipos de token…",
    "menu.settings.syntax.reset_theme": "Restablecer el tema", "menu.settings.syntax.auto": "Resaltar automáticamente al abrir",
    "menu.settings.editor": "Comportamiento del editor", "menu.settings.editor.persistent_sel": "Selección persistente",
    "menu.settings.editor.remember_geom": "Recordar tamaño y posición de la ventana", "menu.settings.editor.coding": "Ayudas de programación",
    "menu.settings.editor.coding.autoindent": "Sangrar automáticamente las líneas nuevas", "menu.settings.editor.coding.soft_tabs": "Tabulaciones suaves (insertar espacios)",
    "menu.settings.editor.coding.hang_indent": "Sangría francesa para líneas ajustadas", "menu.settings.editor.coding.match_brackets": "Resaltar los paréntesis coincidentes",
    "menu.settings.editor.coding.close_brackets": "Cerrar automáticamente paréntesis y comillas", "menu.settings.editor.autosave": "Guardado automático",
    "menu.settings.editor.autosave.off": "Desactivado", "menu.settings.editor.autosave.30s": "Cada 30 s",
    "menu.settings.editor.autosave.1m": "Cada 1 min", "menu.settings.editor.autosave.2m": "Cada 2 min",
    "menu.settings.editor.autosave.5m": "Cada 5 min", "menu.settings.editor.tab_width": "Ancho de tabulación",
    "menu.settings.editor.precision": "Precisión numérica", "menu.settings.editor.precision.off": "Desactivada (completa)",
    "menu.settings.keyboard": "Teclado", "menu.settings.keyboard.load": "Cargar…",
    "menu.settings.keyboard.save": "Guardar…", "menu.settings.keyboard.reset": "Restablecer al teclado integrado",
    "menu.settings.allow_shell": "Permitir comandos de shell", "menu.settings.save_settings": "Guardar configuración…",
    "menu.settings.load_settings": "Cargar configuración…", "menu.settings.autosave_settings": "Guardar la configuración automáticamente",
    # Help
    "menu.help.reference": "Referencia", "menu.help.about": "Acerca de XyScript",
    # Dialogs (wired)
    "button.close": "Cerrar",
    "dialog.grep.title": "Grep", "dialog.grep.pattern": "Buscar:",
    "dialog.grep.pattern_hint": "texto (o una expresión regular con «Expresión regular»)",
    "dialog.grep.folder": "En la carpeta:", "dialog.grep.folder_hint": "vacío = carpeta actual — o  src  ·  *.rs  ·  src/*.rs",
    "dialog.grep.regex": "Expresión regular", "dialog.grep.ignore_case": "Ignorar mayúsculas y minúsculas",
    "dialog.grep.whole_word": "Palabra completa", "dialog.grep.search": "Buscar",
    "dialog.run.title": "Ejecutar comando",
    "dialog.run.shell_off": "Los comandos de shell están desactivados — actívelos en Configuración ⏵ Permitir comandos de shell.",
    "dialog.run.command": "Comando:", "dialog.run.command_hint": "un comando de shell",
    "dialog.run.background": "Ejecutar en segundo plano", "dialog.run.insert_output": "Insertar la salida en el cursor",
    "dialog.run.run": "Ejecutar",
    "dialog.find.find_label": "Buscar:", "dialog.find.find_hint": "texto que buscar",
    "dialog.find.replace_label": "Reemplazar:", "dialog.find.replace_hint": "reemplazo",
    "dialog.find.case": "Distinguir mayúsculas y minúsculas", "dialog.find.whole_word": "Palabra completa",
    "dialog.find.regex": "Expresión regular", "dialog.find.in_selection": "En la selección",
    "dialog.find.keep_case": "Conservar mayúsculas y minúsculas al reemplazar",
    "dialog.find.keep_case_hint": "Reemplazar / Reemplazar todo: ajusta las mayúsculas y minúsculas de cada reemplazo al texto que sustituye (minúsculas / Con inicial mayúscula / MAYÚSCULAS).",
    "dialog.find.find_next": "Siguiente (F3)", "dialog.find.find_prev": "Anterior (Mayús+F3)",
    "dialog.find.replace": "Reemplazar", "dialog.find.replace_all": "Reemplazar todo",
    "dialog.about.title": "Acerca de XyScript", "dialog.about.tagline": "Un editor configurable en la tradición de XyWrite.",
    # Messages (curated phase-2 batch — keep every {placeholder} intact)
    "msg.search.not_found": "no encontrado: {find}",
    "msg.open.not_found": "archivo no encontrado: {path}",
    "msg.save.written": "guardado {name}",
    "msg.readonly": "el documento es de solo lectura (RO para desbloquear)",
    "msg.grep.none": "GREP: sin coincidencias",
    "msg.grep.searching": "GREP: buscando…",
}

HEADER = """\
# =============================================================================
# XyScript UI strings  —  ESPAÑOL (sample translation)
# =============================================================================
# A sample Spanish catalog, generated from xyscript-strings.template. Load it:
#   LANG <path>\\xyscript-strings-es.txt
# Menu labels, dialogs, and the curated status/error messages are all wired
# (phases 1 & 2). Any untranslated key falls back to English. The ~200 XPL
# parser diagnostics stay English by design. {placeholders} kept intact.
# =============================================================================

"""

ENTRY = re.compile(r'^([A-Za-z][\w.]*)\s*=\s*#\s*(.*)$')
out = [re.sub(r'xyscript-strings-[a-z]{2}\.\w+', os.path.basename(DST), HEADER)]
started = False
missing = []
for raw in open(SRC, encoding='utf-8'):
    line = raw.rstrip('\n')
    if not started:
        if line.strip().endswith('1. MENUS'):
            out.append('# =============================================================================')
            out.append(line)
            started = True
        continue
    m = ENTRY.match(line)
    if m:
        key, comment = m.group(1), m.group(2)
        val = ES.get(key, "")
        if not val:
            missing.append(key)
        out.append(f"{key} = {val}   # {comment}")
    else:
        out.append(line)
open(DST, 'w', encoding='utf-8').write('\n'.join(out) + '\n')
print(f"wrote {DST}; {len(missing)} untranslated key(s)" + (": " + ", ".join(missing) if missing else ""))
