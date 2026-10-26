"""Gera a página do fly: início, desempenho e documentação com exemplos executados.

Os textos e os exemplos ficam em content.py. Cada trecho de código roda com o binário do fly durante o
build, e a saída mostrada é a que ele imprimiu. O Node roda os mesmos trechos, e a marca "igual ao Node"
aparece só quando a saída é idêntica. Se um exemplo sair com código diferente do esperado, o build para.

Saídas: artifact.html (publicado, começa no título) e fly-site-v9.html (página completa, para olhar local).

Uso, a partir da raiz do repositório: python3 site/build.py. O binário vem de target/release/fly; a variável FLY aponta para outro.
"""
import html
import json
import os
import re
import subprocess
import sys
import tempfile
import unicodedata

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import content  # noqa: E402

FLY = os.environ.get("FLY", os.path.join(HERE, "..", "target", "release", "fly"))
NODE = os.environ.get("NODE_BIN", "node")

# Medidas de benchmarks/bench.py: fly, Node 22 e Bun 1.4, execuções intercaladas, mediana de 5 execuções.
BENCH = [
    ("array", {"fly": 64.4, "node": 99.6, "bun": 60.0}, "ms"),
    ("binary_trees", {"fly": 106.9, "node": 58.0, "bun": 43.0}, "ms"),
    ("char_codes", {"fly": 68.0, "node": 57.7, "bun": 37.2}, "ms"),
    ("closures", {"fly": 108.3, "node": 41.1, "bun": 60.2}, "ms"),
    ("console_lines", {"fly": 25.9, "node": 441.2, "bun": 109.7}, "ms"),
    ("dict_keys", {"fly": 12.8, "node": 45.3, "bun": 23.9}, "ms"),
    ("exceptions", {"fly": 41.1, "node": 262.0, "bun": 47.4}, "ms"),
    ("fib", {"fly": 9.2, "node": 42.8, "bun": 14.9}, "ms"),
    ("json", {"fly": 159.7, "node": 158.0, "bun": 90.9}, "ms"),
    ("life", {"fly": 1230.2, "node": 200.3, "bun": 142.3}, "ms"),
    ("loop", {"fly": 21.5, "node": 84.1, "bun": 32.2}, "ms"),
    ("mandelbrot", {"fly": 12.0, "node": 43.5, "bun": 17.3}, "ms"),
    ("matrix", {"fly": 74.2, "node": 39.3, "bun": 17.9}, "ms"),
    ("method_calls", {"fly": 133.5, "node": 38.6, "bun": 14.3}, "ms"),
    ("nbody_objects", {"fly": 282.9, "node": 41.9, "bun": 17.0}, "ms"),
    ("num_to_string", {"fly": 101.5, "node": 59.9, "bun": 56.8}, "ms"),
    ("objects", {"fly": 3.6, "node": 34.9, "bun": 20.0}, "ms"),
    ("sieve", {"fly": 355.8, "node": 160.6, "bun": 76.1}, "ms"),
    ("sort_cmp", {"fly": 117.8, "node": 110.3, "bun": 74.2}, "ms"),
    ("split_join", {"fly": 55.5, "node": 86.3, "bun": 48.7}, "ms"),
    ("str_index", {"fly": 8.8, "node": 35.0, "bun": 10.3}, "ms"),
    ("string", {"fly": 9.0, "node": 43.0, "bun": 18.8}, "ms"),
    ("string_builder", {"fly": 54.4, "node": 105.7, "bun": 67.9}, "ms"),
    ("tak", {"fly": 9.4, "node": 41.5, "bun": 17.7}, "ms"),
]
OTHER = [
    ("inicialização", {"fly": 2.7, "node": 34.1, "bun": 6.1}, "ms"),
    ("tamanho do executável", {"fly": 1205, "node": 123405, "bun": 107309}, "KB", {"fly": "1,2 MB", "node": "123 MB", "bun": "107 MB"}),
]
BAR_CLASS = {"fly": "bar-fly", "node": "bar-node", "bun": "bar-bun", "python": "bar-python"}

KEYWORDS = {
    "const", "let", "var", "function", "return", "if", "else", "for", "while", "do", "switch", "case",
    "default", "break", "continue", "try", "catch", "finally", "throw", "new", "typeof", "this", "true",
    "false", "null", "undefined", "of", "in",
}
BUILTINS = {
    "console", "process", "Math", "JSON", "Object", "Array", "Date", "Number", "String", "Boolean", "Error",
    "TypeError", "RangeError", "SyntaxError", "ReferenceError", "require", "parseInt", "parseFloat", "isNaN",
    "isFinite", "NaN", "Infinity", "setTimeout", "setInterval", "setImmediate", "clearTimeout",
    "clearInterval", "clearImmediate", "__dirname", "__filename", "exports", "module",
}
TOKEN = re.compile(
    r"""(?P<com>//[^\n]*)"""
    r"""|(?P<str>"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|`(?:\\.|[^`\\])*`)"""
    r"""|(?P<num>\b\d+(?:\.\d+)?\b)"""
    r"""|(?P<word>[A-Za-zÀ-ÿ_$][A-Za-z0-9_$À-ÿ]*)"""
    r"""|(?P<other>.)""",
    re.S,
)
INLINE = re.compile(r"`([^`]+)`|\*\*([^*]+)\*\*")


def esc(text):
    return html.escape(text, quote=False)


def inline(text):
    """Texto com `código` e **negrito**, escapado."""
    out, pos = [], 0
    for m in INLINE.finditer(text):
        out.append(esc(text[pos:m.start()]))
        if m.group(1) is not None:
            out.append(f"<code>{esc(m.group(1))}</code>")
        else:
            out.append(f"<strong>{esc(m.group(2))}</strong>")
        pos = m.end()
    out.append(esc(text[pos:]))
    return "".join(out)


def plain(text):
    return re.sub(r"[`*]", "", text)


def highlight(code):
    """Colore código JavaScript com as classes tk-* da folha de estilos."""
    out = []
    for m in TOKEN.finditer(code):
        kind, text = m.lastgroup, m.group(0)
        e = esc(text)
        if kind == "com":
            out.append(f'<span class="tk-com">{e}</span>')
        elif kind == "str":
            out.append(f'<span class="tk-str">{e}</span>')
        elif kind == "num":
            out.append(f'<span class="tk-num">{e}</span>')
        elif kind == "word":
            after = code[m.end():m.end() + 1]
            if text in KEYWORDS:
                out.append(f'<span class="tk-kw">{e}</span>')
            elif text in BUILTINS:
                out.append(f'<span class="tk-bi">{e}</span>')
            elif after == "(":
                out.append(f'<span class="tk-fn">{e}</span>')
            else:
                out.append(e)
        else:
            out.append(e)
    return "".join(out)


def shell_block(text):
    lines = []
    for line in text.split("\n"):
        lines.append(f'<span class="tk-pun">$</span> {esc(line)}')
    return "\n".join(lines)


# ----------------------------------------------------------------- execução real
def write_files(folder, files):
    for name, text in files.items():
        path = os.path.join(folder, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(text)


def run(cmd, cwd):
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=30)
    return proc.returncode, proc.stdout, proc.stderr


def scrub(result, folder):
    """Tira o diretório temporário da saída: o nome aleatório mudaria a página a cada build."""
    def clean(text):
        return text.replace(folder + os.sep, "").replace(folder, ".")
    rc, out, err = result
    return rc, clean(out), clean(err)


def run_example(ex):
    """Roda o exemplo com o fly e, se pedido, com o Node no mesmo diretório."""
    with tempfile.TemporaryDirectory() as tmp:
        if "eval" in ex:
            fly_cmd = [FLY, "-e", ex["eval"]]
            node_cmd = [NODE, "-e", ex["eval"]]
        else:
            files = dict(ex.get("files", {}))
            files[ex["file"]] = ex["code"]
            write_files(tmp, files)
            args = ex.get("args", [])
            fly_cmd = [FLY, ex["file"], *args]
            node_cmd = [NODE, ex["file"], *args]
        fly = scrub(run(fly_cmd, tmp), tmp)
        node = scrub(run(node_cmd, tmp), tmp) if ex.get("node", True) else None
    return fly, node


def probe_result(code, runtime):
    """Resultado de um trecho: a saída, ou a primeira linha do erro."""
    with tempfile.TemporaryDirectory() as tmp:
        name = "probe.fly" if runtime == "fly" else "probe.js"
        write_files(tmp, {name: code})
        cmd = [FLY, name] if runtime == "fly" else [NODE, name]
        rc, out, err = scrub(run(cmd, tmp), tmp)
    if rc == 0:
        return out.rstrip() or "(sem saída)"
    first = next((line for line in err.splitlines() if line.strip()), "erro")
    return first.replace("Uncaught ", "", 1)


def pane(name, code, copy=True):
    btn = '<button class="copy" type="button">copiar</button>' if copy else ""
    return (
        '<div class="pane">'
        f'<div class="pane-head"><span class="pane-name">{esc(name)}</span>{btn}</div>'
        f'<pre class="code">{highlight(code)}</pre></div>'
    )


def render_example(ex):
    (fly_rc, fly_out, fly_err), node = run_example(ex)
    expect = ex.get("expect", 0)
    if fly_rc != expect:
        raise SystemExit(f"exemplo {ex['title']!r} saiu com {fly_rc}, esperado {expect}:\n{fly_err}")
    same = bool(node) and node[0] == fly_rc and node[1].rstrip() == fly_out.rstrip()
    if "eval" in ex:
        label = "fly -e"
        main = pane(label, ex["eval"], copy=True)
    else:
        label = " ".join([ex["file"], *ex.get("args", [])])
        main = pane(label, ex["code"])
    extra = "".join(pane(name, text) for name, text in ex.get("files", {}).items())
    marks = []
    if fly_rc != 0:
        marks.append(f'<span class="mark exit">código {fly_rc}</span>')
    if same:
        marks.append('<span class="mark same">igual ao Node</span>')
    text_out = fly_out.rstrip() or "(sem saída)"
    out_cls = "" if fly_out.strip() else ' class="empty"'
    output = (
        '<div class="pane out">'
        f'<div class="pane-head"><span class="pane-name">saída</span><span class="marks">{"".join(marks)}</span></div>'
        f'<pre class="out"{out_cls}>{esc(text_out)}</pre></div>'
    )
    if fly_err.strip() and (ex.get("show_err") or fly_rc != 0):
        output += (
            '<div class="pane err">'
            '<div class="pane-head"><span class="pane-name">erro padrão</span></div>'
            f'<pre class="out err">{esc(fly_err.rstrip())}</pre></div>'
        )
    return (
        '<figure class="ex">'
        '<figcaption class="ex-head">'
        f'<h3>{esc(ex["title"])}</h3>'
        f'<p>{inline(ex["desc"])}</p>'
        "</figcaption>"
        f'<div class="frame">{extra}{main}{output}</div>'
        "</figure>"
    )


def render_probe(mode, rows):
    head = ["trecho", "fly"] + (["Node"] if mode == "both" else [])
    body = []
    for code in rows:
        cells = [f'<td><pre class="mini">{highlight(code)}</pre></td>']
        cells.append(f'<td><pre class="mini">{esc(probe_result(code, "fly"))}</pre></td>')
        if mode == "both":
            cells.append(f'<td><pre class="mini">{esc(probe_result(code, "node"))}</pre></td>')
        body.append("<tr>" + "".join(cells) + "</tr>")
    thead = "".join(f"<th>{esc(h)}</th>" for h in head)
    return (
        '<div class="table-scroll"><table>'
        f"<thead><tr>{thead}</tr></thead><tbody>{''.join(body)}</tbody></table></div>"
    )


# ----------------------------------------------------------------- páginas
ORDER = [slug for _, slugs in content.GROUPS for slug in slugs]
TITLES = {slug: content.PAGES[slug]["title"] for slug in ORDER}


def render_table(headers, rows):
    thead = "".join(f"<th>{inline(h)}</th>" for h in headers)
    tbody = "".join("<tr>" + "".join(f"<td>{inline(c)}</td>" for c in row) + "</tr>" for row in rows)
    return f'<div class="table-scroll"><table><thead><tr>{thead}</tr></thead><tbody>{tbody}</tbody></table></div>'


def page_article(slug):
    """Um artigo da documentação, com o índice da página e o texto para a busca."""
    p = content.PAGES[slug]
    parts, toc, index = [], [], []
    open_section = False

    def close():
        nonlocal open_section
        if open_section:
            parts.append("</section>")
            open_section = False

    def start(title, sid):
        nonlocal open_section
        close()
        anchor = f' id="{sid}"' if sid else ""
        parts.append(f'<section class="sec"{anchor}><h2>{esc(title)}</h2>' if sid else '<section class="sec">')
        open_section = True
        if sid:
            toc.append((sid, title))
            index.append({"page": p["title"], "section": title, "href": f"#{sid}", "text": ""})
        else:
            index.append({"page": p["title"], "section": p["title"], "href": f"#{slug}", "text": ""})

    start_index_needed = True
    for blk in p["blocks"]:
        kind = blk[0]
        if kind == "h2":
            start(blk[1], f"{slug}.{blk[2]}")
            start_index_needed = False
            continue
        if start_index_needed:
            start(p["title"], None)
            start_index_needed = False
        entry = index[-1]
        if kind == "p":
            parts.append(f"<p>{inline(blk[1])}</p>")
            entry["text"] += " " + plain(blk[1])
        elif kind == "ul":
            parts.append('<ul class="list">' + "".join(f"<li>{inline(t)}</li>" for t in blk[1]) + "</ul>")
            entry["text"] += " " + " ".join(plain(t) for t in blk[1])
        elif kind == "shell":
            parts.append(f'<div class="frame shell"><div class="pane"><div class="pane-head"><span class="pane-name">terminal</span>'
                         f'<button class="copy" type="button">copiar</button></div><pre class="code">{shell_block(blk[1])}</pre></div></div>')
            entry["text"] += " " + blk[1]
        elif kind == "ex":
            ex = blk[1]
            parts.append(render_example(ex))
            entry["text"] += " " + ex["title"] + " " + ex["desc"] + " " + ex.get("code", ex.get("eval", ""))
        elif kind == "note":
            parts.append(f'<aside class="note"><p class="note-label">{esc(blk[1])}</p><p>{inline(blk[2])}</p></aside>')
            entry["text"] += " " + plain(blk[2])
        elif kind == "table":
            parts.append(render_table(blk[1], blk[2]))
            entry["text"] += " " + " ".join(plain(c) for row in blk[2] for c in row)
        elif kind == "probe":
            parts.append(render_probe(blk[1], blk[2]))
            entry["text"] += " " + " ".join(blk[2])
        else:
            raise SystemExit(f"bloco desconhecido em {slug}: {kind}")
    close()

    pos = ORDER.index(slug)
    prev_slug = ORDER[pos - 1] if pos > 0 else None
    next_slug = ORDER[pos + 1] if pos + 1 < len(ORDER) else None
    pager = ['<nav class="pager" aria-label="páginas da documentação">']
    if prev_slug:
        pager.append(f'<a href="#{prev_slug}" rel="prev"><span class="dir">anterior</span>{esc(TITLES[prev_slug])}</a>')
    if next_slug:
        pager.append(f'<a class="next" href="#{next_slug}" rel="next"><span class="dir">próxima</span>{esc(TITLES[next_slug])}</a>')
    pager.append("</nav>")

    group = next(name for name, slugs in content.GROUPS if slug in slugs)
    toc_html = ""
    if toc:
        items = "".join(f'<li><a href="#{sid}">{esc(title)}</a></li>' for sid, title in toc)
        toc_html = f'<aside class="toc" aria-label="nesta página"><p class="toc-title">nesta página</p><ol>{items}</ol></aside>'
    article = (
        f'<article class="doc" id="{slug}" hidden>'
        '<header class="doc-head">'
        f'<p class="crumb">documentação / {esc(group)}</p>'
        f'<h1>{esc(p["title"])}</h1>'
        f'<p class="lede">{inline(p["lede"])}</p>'
        "</header>"
        '<div class="doc-grid">'
        f'<div class="prose">{"".join(parts)}{"".join(pager)}</div>'
        f"{toc_html}"
        "</div></article>"
    )
    for entry in index:
        entry["text"] = re.sub(r"\s+", " ", entry["text"]).strip()[:900]
    return article, index


def side_html():
    groups = []
    for name, slugs in content.GROUPS:
        links = "".join(f'<li><a href="#{s}" data-page="{s}">{esc(TITLES[s])}</a></li>' for s in slugs)
        groups.append(f'<p class="group-label">{esc(name)}</p><ul>{links}</ul>')
    return (
        '<aside class="side" aria-label="índice da documentação">'
        '<details class="side-box" open><summary>índice da documentação</summary>'
        '<label class="search"><span class="sr-only">buscar na documentação</span>'
        '<input id="q" type="search" placeholder="buscar na documentação" autocomplete="off"></label>'
        '<div id="results" class="results" hidden></div>'
        f'<nav id="side-nav" class="side-nav" aria-label="páginas">{"".join(groups)}</nav>'
        "</details></aside>"
    )


# ----------------------------------------------------------------- início
def home_html():
    h = content.HOME
    badges = "".join(f'<span class="badge">{esc(b)}</span>' for b in h["badges"])
    resumo = "".join(
        f'<div class="resumo-item"><h3>{esc(label)}</h3><p>{inline(text)}</p></div>'
        for label, text in h["resumo"]
    )
    topics = "".join(
        f'<a class="topic" href="#{slug}"><span class="topic-title">{esc(title)}</span>'
        f'<span class="topic-desc">{esc(desc)}</span></a>'
        for slug, title, desc in content.TOPICS
    )
    limites = "".join(f"<li>{inline(t)}</li>" for t in h["limites"])
    return f"""<div class="view wrap" data-view="inicio">
<section class="hero">
  <div class="hero-art">
    <canvas id="signal" width="720" height="720" role="img" aria-label="borboleta em 3D, girando em linhas de varredura"></canvas>
  </div>
  <div class="hero-copy">
    <div class="badges">{badges}</div>
    <h1>fly</h1>
    <p class="lede">{esc(h["lede"])}</p>
    <div class="cta">
      <a class="btn primary" href="#docs-comecar">começar</a>
      <a class="btn" href="#docs">documentação</a>
      <a class="btn quiet" href="https://github.com/davixyzc/New-Project" target="_blank" rel="noopener">código no GitHub</a>
    </div>
  </div>
</section>

<section class="block" id="resumo">
  <h2>Em resumo</h2>
  <div class="resumo">{resumo}</div>
</section>

<section class="block" id="desempenho">
  <h2>Desempenho</h2>
  <p class="block-lede">{esc(h["desempenho"])}</p>
  <div class="bench">
    <div class="bench-cards">{bench_card("tempo de parede", BENCH)}{bench_card("outras medidas", OTHER)}</div>
    <div class="como-ler">
      <p class="label">como ler</p>
      <p>{esc(h["como_ler"])}</p>
      <p><a href="#docs-execucao">Como o fly executa</a> explica de onde vem essa diferença.</p>
    </div>
  </div>
</section>

<section class="block" id="documentacao">
  <h2>Documentação</h2>
  <p class="block-lede">Cada página explica um assunto, com exemplos que rodam de verdade. As saídas mostradas são as do binário, e os exemplos que também dão o mesmo resultado no Node têm essa marca.</p>
  <div class="topics">{topics}</div>
</section>

<section class="block" id="limites">
  <h2>Limites</h2>
  <ul class="limits">{limites}</ul>
  <p><a href="#docs-diferencas">Ver todas as diferenças do JavaScript</a></p>
</section>
</div>"""


def bench_card(title, rows):
    groups = []
    for row in rows:
        label, values, unit = row[:3]
        overrides = row[3] if len(row) > 3 else {}
        top = max(values.values())
        bars = []
        for name in ("fly", "node", "bun", "python"):
            if name not in values:
                continue
            value = values[name]
            shown = f"{value:,.0f}".replace(",", ".") if value >= 1000 else f"{value:g}".replace(".", ",")
            text = overrides.get(name, f"{shown} {unit}")
            width = value / top * 100
            bars.append(
                '<div class="bar-row">'
                f'<span class="bar-name">{name}</span>'
                f'<span class="bar-track"><span class="bar {BAR_CLASS[name]}" style="width:{width:.1f}%"></span></span>'
                f'<span class="bar-val">{esc(text)}</span>'
                "</div>"
            )
        groups.append(
            '<div class="bench-group">'
            f'<p class="bench-title">{esc(label)}</p>' + "".join(bars) + "</div>"
        )
    return (
        '<article class="card">'
        f'<header class="card-head">{esc(title)}</header>'
        f'<div>{"".join(groups)}</div></article>'
    )


# ----------------------------------------------------------------- casca
def header_html():
    return (
        '<header class="top"><div class="top-in">'
        '<a class="brand" href="#inicio" aria-label="fly, início">fly<small>0.1.0</small></a>'
        '<nav class="top-nav" aria-label="principal">'
        '<a href="#inicio" data-top="inicio">início</a>'
        '<a href="#docs" data-top="docs">documentação</a>'
        '<a href="#desempenho">desempenho</a>'
        "</nav>"
        '<button class="theme" id="theme" type="button">tema: sistema</button>'
        "</div></header>"
    )


def footer_html():
    return (
        '<footer class="foot"><div class="foot-in">'
        "<span>fly · linguagem e runtime em Rust</span>"
        '<a href="https://github.com/davixyzc/New-Project" target="_blank" rel="noopener">código no GitHub</a>'
        "</div></footer>"
    )


def docs_html(articles, index_entries):
    shell = (
        '<div class="view wrap doc-shell" data-view="docs" hidden>'
        f"{side_html()}"
        f'<div class="doc-main">{"".join(articles)}</div>'
        "</div>"
    )
    return shell


def json_script(id_, data):
    payload = json.dumps(data, ensure_ascii=False).replace("<", "\\u003c")
    return f'<script type="application/json" id="{id_}">{payload}</script>'


def build():
    read = lambda name: open(os.path.join(HERE, name), encoding="utf-8").read()  # noqa: E731
    css = read("style.css")
    js = read("app.js")
    with open(os.path.join(HERE, "frames.json"), encoding="utf-8") as fh:
        frames = json.load(fh)

    articles, index = [], []
    for slug in ORDER:
        article, entries = page_article(slug)
        articles.append(article)
        for entry in entries:
            entry["page_slug"] = slug
        index.extend(entries)

    body = (
        header_html()
        + '<main id="app">'
        + home_html()
        + docs_html(articles, index)
        + "</main>"
        + footer_html()
        + json_script("frames", frames)
        + json_script("search-index", index)
        + f"\n<script>\n{js}\n</script>\n"
    )
    fonts = (
        '<link rel="preconnect" href="https://fonts.googleapis.com">\n'
        '<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>\n'
        '<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;600'
        "&family=IBM+Plex+Sans:wght@400;500;600;700&family=Silkscreen:wght@400;700&display=swap\">\n"
    )
    artifact = (
        "<title>Linguagem fly</title>\n"
        '<meta name="claude:viewport-fit-sides" content="cover">\n'
        + fonts
        + f"<style>\n{css}\n</style>\n"
        + body
    )
    with open(os.path.join(HERE, "artifact.html"), "w", encoding="utf-8") as fh:
        fh.write(artifact)

    page = (
        '<!doctype html>\n<html lang="pt-BR">\n<head>\n<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n'
        "<title>Linguagem fly</title>\n"
        '<meta name="description" content="fly: sintaxe de JavaScript e modelo do Node.js, em um binário de 963 KB, com documentação e exemplos executados.">\n'
        + fonts
        + f"<style>\n{css}\n</style>\n</head>\n<body>\n"
        + body
        + "</body>\n</html>\n"
    )
    out_path = os.path.join(HERE, "fly-site-v9.html")
    with open(out_path, "w", encoding="utf-8") as fh:
        fh.write(page)
    examples = sum(1 for slug in ORDER for blk in content.PAGES[slug]["blocks"] if blk[0] == "ex")
    print(f"wrote artifact.html ({len(artifact.encode('utf-8')) / 1024:.1f} KB) and fly-site-v9.html; "
          f"{len(ORDER)} páginas, {examples} exemplos executados, {len(index)} seções na busca")


if __name__ == "__main__":
    build()
