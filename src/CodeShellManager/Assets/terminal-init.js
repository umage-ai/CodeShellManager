  const term = new Terminal(Object.assign({
    cursorBlink: true,
    fontSize: 14,
    fontFamily: "'Cascadia Code', 'Cascadia Mono', Consolas, 'Courier New', monospace",
    fontLigatures: true,
    theme: {
      background: '#1e1e1e',
      foreground: '#d4d4d4',
      cursor: '#d4d4d4',
      selectionBackground: '#264f78',
      black: '#1e1e1e',   brightBlack: '#808080',
      red: '#f44747',     brightRed: '#f44747',
      green: '#608b4e',   brightGreen: '#608b4e',
      yellow: '#dcdcaa',  brightYellow: '#dcdcaa',
      blue: '#569cd6',    brightBlue: '#569cd6',
      magenta: '#c678dd', brightMagenta: '#c678dd',
      cyan: '#4ec9b0',    brightCyan: '#4ec9b0',
      white: '#d4d4d4',   brightWhite: '#ffffff'
    },
    scrollback: 5000,
    allowProposedApi: true,
    // Render box-drawing and block characters with built-in glyphs so they
    // tile flush across cells — the font's own glyphs leave thin seams.
    customGlyphs: true,
    // Ctrl+C/V handled via customKeyEventHandler below. Ctrl+Shift+C/V also work.
    macOptionIsMeta: false,
    windowsMode: true,   // correct line ending handling on Windows
  }, window.__termOptions || {}));

  const fitAddon = new FitAddon.FitAddon();
  term.loadAddon(fitAddon);
  term.open(document.getElementById('terminal'));
  fitAddon.fit();

  // ── Input → PTY ────────────────────────────────────────────────────────────
  function sendInput(data) {
    window.chrome.webview.postMessage(JSON.stringify({ type: 'input', data }));
  }

  term.onData(data => sendInput(data));

  // ── Resize notification ────────────────────────────────────────────────────
  term.onResize(({ cols, rows }) => {
    window.chrome.webview.postMessage(JSON.stringify({ type: 'resize', cols, rows }));
  });

  // ── Messages from WPF ──────────────────────────────────────────────────────
  window.chrome.webview.addEventListener('message', e => {
    try {
      const msg = JSON.parse(e.data);
      if      (msg.type === 'output')         term.write(msg.data);
      else if (msg.type === 'clear')          term.clear();
      else if (msg.type === 'focus')          { term.focus(); fitAddon.fit(); }
      else if (msg.type === 'fit')            { fitAddon.fit(); term.focus(); }
      else if (msg.type === 'paste')          term.paste(msg.data);
      else if (msg.type === 'setOptions')     {
        const opts = msg.options;
        if (opts.fontFamily    !== undefined) term.options.fontFamily    = opts.fontFamily;
        if (opts.fontSize      !== undefined) term.options.fontSize      = opts.fontSize;
        if (opts.fontLigatures !== undefined) term.options.fontLigatures = opts.fontLigatures;
        if (opts.fontWeight    !== undefined) term.options.fontWeight    = opts.fontWeight;
        if (opts.letterSpacing !== undefined) term.options.letterSpacing = opts.letterSpacing;
        if (opts.lineHeight    !== undefined) term.options.lineHeight    = opts.lineHeight;
        if (opts.theme         !== undefined) term.options.theme         = opts.theme;
        if (opts.cursorStyle   !== undefined) term.options.cursorStyle   = opts.cursorStyle;
        if (opts.cursorBlink   !== undefined) term.options.cursorBlink   = opts.cursorBlink;
        if (opts.padding       !== undefined) document.getElementById('terminal').style.padding = opts.padding;
        if (opts.retro         !== undefined) document.body.classList.toggle('retro', !!opts.retro);
        fitAddon.fit();
      }
      else if (msg.type === 'dropOverlayClear') overlay.classList.remove('active');
      else if (msg.type === 'setBootState') {
        const label = document.getElementById('bootLabel');
        const spinner = document.getElementById('bootSpinner');
        if (label && typeof msg.label === 'string') label.textContent = msg.label;
        if (spinner && typeof msg.accentHex === 'string') {
          spinner.style.setProperty('--boot-accent', msg.accentHex);
        }
      }
      else if (msg.type === 'bootDone') {
        const overlay = document.getElementById('bootOverlay');
        if (overlay && !overlay.classList.contains('hidden')) {
          overlay.classList.add('hidden');
          overlay.addEventListener('transitionend', () => {
            try { overlay.parentNode && overlay.parentNode.removeChild(overlay); } catch {}
          }, { once: true });
        }
      }
    } catch {}
  });

  // ── Clipboard: all copy/paste routes through WPF ──────────────────────────
  // customKeyEventHandler returning false stops xterm's own key handling but
  // does NOT call preventDefault() on the DOM event, so Chromium can still fire
  // a native 'paste' event that xterm catches via its internal textarea listener.
  // We block that at the capture phase below.
  term.attachCustomKeyEventHandler(e => {
    if (e.type !== 'keydown' || !e.ctrlKey) return true;
    if (!e.shiftKey) {
      if (e.key === 'c') {
        const sel = term.getSelection();
        if (sel) {
          window.chrome.webview.postMessage(JSON.stringify({ type: 'setClipboard', text: sel }));
          term.clearSelection();
          return false; // swallow — don't send ^C to PTY
        }
      }
      if (e.key === 'v') {
        window.chrome.webview.postMessage(JSON.stringify({ type: 'getClipboard' }));
        return false; // swallow — don't send ^V to PTY
      }
    } else {
      // Ctrl+Shift+V/C: document keydown listener handles sending the message;
      // return false here so xterm doesn't also paste/copy natively.
      if (e.key === 'V' || e.key === 'C') return false;
    }
    return true;
  });

  // Block native paste events before xterm's own paste listeners (registered on
  // both the hidden textarea AND the terminal element) can read clipboardData
  // and send it as input. preventDefault alone cancels the browser's default
  // text insertion but doesn't stop those listeners — stopImmediatePropagation
  // during capture phase prevents the event from reaching them at all.
  // All pasting is routed through our getClipboard → WPF → PTY path.
  document.addEventListener('paste', e => {
    e.preventDefault();
    e.stopImmediatePropagation();
  }, { capture: true });

  // ── Right-click: paste from clipboard ─────────────────────────────────────
  document.getElementById('terminal').addEventListener('contextmenu', async e => {
    e.preventDefault();
    try {
      window.chrome.webview.postMessage(JSON.stringify({ type: 'getClipboard' }));
    } catch {}
  });

  // ── Ctrl+Shift+V / Ctrl+Shift+C ───────────────────────────────────────────
  document.addEventListener('keydown', e => {
    if (e.ctrlKey && e.shiftKey && e.key === 'V') {
      e.preventDefault();
      window.chrome.webview.postMessage(JSON.stringify({ type: 'getClipboard' }));
    }
    if (e.ctrlKey && e.shiftKey && e.key === 'C') {
      e.preventDefault();
      const sel = term.getSelection();
      if (sel) {
        window.chrome.webview.postMessage(JSON.stringify({ type: 'setClipboard', text: sel }));
      }
    }
  });

  // ── File drag-and-drop ─────────────────────────────────────────────────────
  // WebView2 exposes dropped files via the DataTransfer API.
  // We send the file paths to WPF which resolves them to Windows paths.
  const overlay = document.getElementById('dropOverlay');

  document.addEventListener('dragenter', e => {
    if (e.dataTransfer.types.includes('Files')) {
      overlay.classList.add('active');
      e.preventDefault();
    }
  });

  document.addEventListener('dragover', e => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
  });

  document.addEventListener('dragleave', e => {
    // Only hide if leaving the window entirely
    if (!e.relatedTarget) overlay.classList.remove('active');
  });

  document.addEventListener('drop', e => {
    e.preventDefault();
    overlay.classList.remove('active');

    // text/uri-list contains full file:// URIs when dragging from Explorer
    const uriList = e.dataTransfer.getData('text/uri-list');
    if (uriList) {
      const paths = uriList.split(/\r?\n/)
        .filter(line => !line.startsWith('#') && line.trim())
        .map(uri => {
          try {
            const url = new URL(uri.trim());
            if (url.protocol === 'file:') {
              // file:///C:/path/to/file → C:\path\to\file
              return decodeURIComponent(url.pathname.replace(/^\//, '').replace(/\//g, '\\'));
            }
          } catch {}
          return null;
        })
        .filter(Boolean);
      if (paths.length > 0) {
        window.chrome.webview.postMessage(JSON.stringify({ type: 'filesDropped', paths }));
        return;
      }
    }

    // Fallback: send names only (WPF OLE handler may resolve paths)
    const files = Array.from(e.dataTransfer.files);
    if (files.length > 0) {
      window.chrome.webview.postMessage(JSON.stringify({
        type: 'filesDropped',
        names: files.map(f => f.name)
      }));
    }
  });

  // ── Fit on resize ──────────────────────────────────────────────────────────
  const resizeObserver = new ResizeObserver(() => {
    try { fitAddon.fit(); } catch {}
  });
  resizeObserver.observe(document.getElementById('terminal'));

  // Initial fit may have run while the WebView2 container was Collapsed (0×0).
  // Re-fit after a short delay so xterm picks up the real dimensions once visible.
  setTimeout(() => { try { fitAddon.fit(); term.focus(); } catch {} }, 50);
  setTimeout(() => { try { fitAddon.fit(); } catch {} }, 250);

  term.focus();
