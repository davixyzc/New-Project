(() => {
  const root = document.documentElement;
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const wide = window.matchMedia('(min-width: 1024px)');
  const $ = (sel) => document.querySelector(sel);
  const $$ = (sel) => Array.from(document.querySelectorAll(sel));
  const setCurrent = (links, isCurrent) => {
    for (const a of links) {
      if (isCurrent(a)) a.setAttribute('aria-current', 'page');
      else a.removeAttribute('aria-current');
    }
  };

  // Preferências deste navegador. O armazenamento pode estar bloqueado ou lançar erro, então cada acesso é protegido.
  const store = {
    get(key) {
      try { return window.localStorage.getItem(key); } catch (e) { return null; }
    },
    set(key, value) {
      try { window.localStorage.setItem(key, value); } catch (e) { /* a escolha vale só nesta visita */ }
    },
  };

  const header = $('.top');
  const homeView = $('[data-view="inicio"]');
  const docsView = $('[data-view="docs"]');
  const sideBox = $('.side-box');
  const sideNav = $('#side-nav');
  const query = $('#q');
  const results = $('#results');
  const topLinks = $$('.top-nav a[data-top]');
  const pageLinks = $$('#side-nav a[data-page]');
  const articles = new Map($$('article.doc').map((a) => [a.id, a]));
  const FIRST = articles.keys().next().value;
  const HOME_ANCHORS = new Set(['resumo', 'desempenho', 'documentacao', 'limites']);

  // A altura real do cabeçalho fixo, que no celular quebra em duas linhas. Âncoras e o índice usam essa margem.
  const setHead = () => root.style.setProperty('--head', `${header.offsetHeight}px`);
  setHead();
  if (window.ResizeObserver) new ResizeObserver(setHead).observe(header);
  else window.addEventListener('resize', setHead);

  // Tema: sistema (o padrão, que segue o aparelho), escuro ou claro.
  const THEMES = ['sistema', 'escuro', 'claro'];
  const themeBtn = $('#theme');
  const saved = store.get('fly-tema');
  let theme = THEMES.includes(saved) ? saved : 'sistema';
  let redrawSignal = () => {};
  const applyTheme = () => {
    if (theme === 'escuro') root.dataset.theme = 'dark';
    else if (theme === 'claro') root.dataset.theme = 'light';
    else delete root.dataset.theme;
    themeBtn.textContent = `tema: ${theme}`;
    redrawSignal();
  };
  themeBtn.addEventListener('click', () => {
    theme = THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length];
    store.set('fly-tema', theme);
    applyTheme();
  });
  applyTheme();

  // Borboleta do início: quadros pré-renderizados, desenhados como linhas de varredura num canvas transparente.
  const canvas = $('#signal');
  const GRID_W = 72;
  const GRID_H = 36;
  const FPS = 12;
  const LINES = 132; // linhas de varredura
  const STEP = 3;    // passo horizontal da amostragem, em pixels
  const frames = JSON.parse($('#frames').textContent).map((hex) => {
    const cells = new Float32Array(GRID_W * GRID_H);
    for (let i = 0; i < cells.length; i++) cells[i] = parseInt(hex[i], 16) / 15;
    return cells;
  });
  const ctx = canvas.getContext('2d');
  const size = canvas.width;
  const pitch = size / LINES;
  const ink = () => getComputedStyle(root).getPropertyValue('--signal').trim() || '#161c2a';

  // Cada linha de varredura engrossa onde o quadro é mais claro, e as células mais claras viram blocos cheios.
  const paint = (cells, alpha, shift, color) => {
    ctx.globalAlpha = alpha;
    ctx.fillStyle = color;
    for (let k = 0; k < LINES; k++) {
      const y = (k + 0.5) * pitch;
      const row = Math.min(GRID_H - 1, Math.floor((y / size) * GRID_H)) * GRID_W;
      for (let x = 0; x < size; x += STEP) {
        const col = Math.min(GRID_W - 1, Math.floor((x / size) * GRID_W));
        const h = pitch * Math.min(1, cells[row + col] * 1.25);
        if (h >= 0.6) ctx.fillRect(x + shift, y - h / 2, STEP, h);
      }
    }
    ctx.globalAlpha = 1;
  };
  let index = 0;
  let previous = null;
  const draw = () => {
    ctx.clearRect(0, 0, size, size);
    const color = ink();
    if (previous !== null) paint(frames[previous], 0.1, 6, color); // um fantasma do quadro anterior
    paint(frames[index], 0.85, 0, color);
  };
  redrawSignal = draw;
  if (reduce) {
    draw();
  } else {
    let last = 0;
    const tick = (now) => {
      if (now - last >= 1000 / FPS && !homeView.hidden && !document.hidden) {
        last = now;
        previous = index;
        index = (index + 1) % frames.length;
        draw();
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  }
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', draw);

  // Copiar. O navegador só aceita a cópia dentro do clique; quando recusa, o código fica selecionado.
  document.addEventListener('click', async (ev) => {
    const btn = ev.target.closest('button.copy');
    if (!btn) return;
    const pane = btn.closest('.pane');
    const block = pane.querySelector('pre');
    const terminal = pane.querySelector('.pane-name').textContent === 'terminal';
    const text = terminal ? block.textContent.replace(/^\$ /gm, '') : block.textContent;
    try {
      await navigator.clipboard.writeText(text);
      btn.textContent = 'copiado';
    } catch (e) {
      const range = document.createRange();
      range.selectNodeContents(block);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      btn.textContent = 'selecionado';
    }
    setTimeout(() => { btn.textContent = 'copiar'; }, 1600);
  });

  // Busca na documentação, sem diferença entre maiúsculas e sem acento. Cada seção do índice guarda o próprio texto.
  const fold = (s) => s.normalize('NFD').replace(/[̀-ͯ]/g, '').toLowerCase();
  const entries = JSON.parse($('#search-index').textContent).map((e) => ({
    ...e,
    key: fold(`${e.page} ${e.section} ${e.text}`),
    sec: fold(e.section),
  }));
  const runSearch = () => {
    const words = fold(query.value).trim().split(/\s+/).filter(Boolean);
    results.replaceChildren();
    results.hidden = words.length === 0;
    sideNav.hidden = words.length > 0;
    if (!words.length) return;
    const inSection = (e) => (words.every((w) => e.sec.includes(w)) ? 1 : 0);
    const hits = entries
      .filter((e) => words.every((w) => e.key.includes(w)))
      .sort((a, b) => inSection(b) - inSection(a))
      .slice(0, 12);
    if (!hits.length) {
      const none = document.createElement('p');
      none.className = 'none';
      none.textContent = 'nenhum resultado';
      results.append(none);
      return;
    }
    for (const e of hits) {
      const link = document.createElement('a');
      link.href = e.href;
      const page = document.createElement('span');
      page.className = 'r-page';
      page.textContent = e.page;
      const section = document.createElement('span');
      section.className = 'r-sec';
      section.textContent = e.section;
      link.append(page, section);
      results.append(link);
    }
  };
  query.addEventListener('input', runSearch);
  query.addEventListener('keydown', (ev) => {
    if (ev.key === 'Escape' && query.value) {
      query.value = '';
      runSearch();
    }
  });

  // O índice começa aberto na lateral larga e fechado no celular.
  sideBox.open = wide.matches;
  wide.addEventListener('change', (ev) => {
    if (ev.matches) sideBox.open = true;
  });

  // Visões: o início, ou a documentação com um artigo aberto. O token vem do hash, como #docs-sintaxe.variaveis.
  const showHome = () => {
    homeView.hidden = false;
    docsView.hidden = true;
    setCurrent(topLinks, (a) => a.dataset.top === 'inicio');
  };
  const showDocs = (slug) => {
    homeView.hidden = true;
    docsView.hidden = false;
    for (const [id, article] of articles) article.hidden = id !== slug;
    setCurrent(topLinks, (a) => a.dataset.top === 'docs');
    setCurrent(pageLinks, (a) => a.dataset.page === slug);
    if (!wide.matches) sideBox.open = false; // no celular, o índice não cobre o texto
  };
  const route = () => {
    let token = '';
    try {
      token = decodeURIComponent(window.location.hash.slice(1));
    } catch (e) {
      token = '';
    }
    if (token === 'docs') token = FIRST;
    const dot = token.indexOf('.');
    const slug = dot === -1 ? token : token.slice(0, dot);
    if (articles.has(slug)) {
      showDocs(slug);
      const target = document.getElementById(token) || articles.get(slug);
      target.scrollIntoView();
      return;
    }
    showHome();
    if (token === 'inicio') window.scrollTo(0, 0);
    const anchor = HOME_ANCHORS.has(token) ? document.getElementById(token) : null;
    if (anchor) anchor.scrollIntoView();
  };
  window.addEventListener('hashchange', route);
  route();
})();
