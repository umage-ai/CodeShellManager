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

  // ── Size reporting: registered BEFORE the first fit, and never only via onResize ──
  //
  // Both halves below are load-bearing, and the second is the non-obvious one.
  //
  //   * onResize used to be registered ~70 lines further down, AFTER the initial
  //     fit(). That fit is the one call that genuinely changes the size — xterm is
  //     constructed at its 80x24 default and fit() measures the real pane — so it
  //     fired the event with no listener attached, and the host never learned the
  //     size at all.
  //
  //   * Moving the registration up is still not sufficient. FitAddon.fit() skips
  //     term.resize() outright when the proposed dimensions already match, and
  //     Terminal.resize() early-returns on an unchanged size. So once the first fit
  //     has landed, EVERY later fit is a silent no-op: the 50ms/250ms timeouts,
  //     document.fonts.ready, the ResizeObserver, and the host's own "fit" message
  //     included. Only a real change in ELEMENT size — resizing the window,
  //     switching layout — ever produced another event.
  //
  // Left at the host's (80, 24) initializer, the ConPTY was created 80 columns wide
  // while xterm drew ~220, so a full-screen TUI like Claude Code painted its frame
  // into roughly a sixth of the pane and stayed that way until the user resized
  // something by hand. postSize() reports the measured size directly and dedupes
  // against the last pair, so a no-op fit still re-syncs the host exactly once.
  var lastPostedCols = -1, lastPostedRows = -1;

  // Mirrors the token the host stamps on each setOptions message, and is echoed back on
  // every size report. It is how the host can tell "the size measured with the font you
  // just asked for" from "the size measured before it" — see WaitForInitialSizeAsync.
  // Promoted in settle() once the new metrics are the ones being measured, NOT when the
  // message arrives: stamping it on arrival makes every report in between claim a font it
  // was not measured with, which is the whole failure the token exists to catch.
  var optionsToken = 0;

  // force: report even when the size is unchanged. Needed to ACK an options token, since
  // a font change that happens not to alter the column count would otherwise be silent
  // and leave the host waiting for a report that never comes.
  function postSize(force) {
    if (!force && term.cols === lastPostedCols && term.rows === lastPostedRows) return;
    try {
      window.chrome.webview.postMessage(JSON.stringify({
        type: 'resize', cols: term.cols, rows: term.rows, token: optionsToken
      }));
    } catch (e) {
      // Leave the dedupe pair unset so the next fit retries. Recording a size we failed
      // to deliver is the same "host never learns the size" bug with a narrower trigger.
      return;
    }
    lastPostedCols = term.cols;
    lastPostedRows = term.rows;
  }

  // Every fit in this file goes through doFit(). Resist calling fitAddon.fit()
  // directly — that is the shape that loses the size report.
  //
  // The measurability guard is not optional. FitAddon.proposeDimensions() bails out
  // when the cell metrics are still 0 (nothing rendered yet) and otherwise clamps its
  // answer to Math.max(2, …) / Math.max(1, …). So on a pane whose container is 0x0 —
  // exactly the case the 50ms/250ms fallbacks below exist for — a fit does nothing and
  // a report would hand the host either xterm's untouched 80x24 default or a 2x1 clamp.
  // Since the host now CREATES the ConPTY from the first size it is told, reporting
  // either would be worse than reporting nothing: pre-fix those were merely dropped.
  function doFit(force) {
    var dims = null;
    try { dims = fitAddon.proposeDimensions(); } catch (e) {}
    if (!dims || isNaN(dims.cols) || isNaN(dims.rows)) return;
    var parent = term.element && term.element.parentElement;
    if (parent && (parent.clientWidth < 1 || parent.clientHeight < 1)) return;
    try { fitAddon.fit(); } catch (e) {}
    postSize(force);
  }

  // Still worth keeping alongside doFit(): a resize can also originate inside the
  // terminal (CSI 8 t) rather than from a fit of ours.
  term.onResize(postSize);

  term.open(document.getElementById('terminal'));
  doFit();

  // ── Shell integration: OSC 9001;key=value;key=value;ST ─────────────────────
  // A program inside the terminal can push session state up to CSM by emitting:
  //   ESC ] 9001 ; color=#89b4fa ; git-branch=main ; git-dirty=1 ; title=foo  ST
  // Recognised keys: color, git-branch, git-dirty (0/1), title.
  // Returning true tells xterm we consumed the sequence so it isn't rendered.
  term.parser.registerOscHandler(9001, data => {
    try {
      const fields = {};
      for (const part of String(data).split(';')) {
        const eq = part.indexOf('=');
        if (eq > 0) fields[part.slice(0, eq).trim()] = part.slice(eq + 1).trim();
      }
      window.chrome.webview.postMessage(JSON.stringify({
        type: 'shellIntegration', fields
      }));
    } catch {}
    return true;
  });

  // ── Input → PTY ────────────────────────────────────────────────────────────
  function sendInput(data) {
    window.chrome.webview.postMessage(JSON.stringify({ type: 'input', data }));
  }

  term.onData(data => sendInput(data));

  // ── "the user actually typed here" signal ──────────────────────────────────
  // Distinct from onData on purpose. onData carries everything xterm sends to the
  // PTY, including replies the TERMINAL generates by itself: device-attribute
  // answers (ESC[?1;2c, ESC[?6c, ESC[>85;95;0c), cursor-position reports, OSC
  // colour replies, and focus in/out (ESC[I / ESC[O). Those are indistinguishable
  // from typing by inspecting the bytes — xterm knows which is which internally
  // (triggerDataEvent's wasUserInput flag) but does not surface it on onData.
  //
  // onKey fires only for real key events, so it is the honest source for "make
  // this pane active". Throttled because promotion is idempotent on the C# side
  // and there is no reason to post on every character.
  var lastKeyPost = 0;
  term.onKey(() => {
    var now = Date.now();
    if (now - lastKeyPost < 500) return;
    lastKeyPost = now;
    window.chrome.webview.postMessage(JSON.stringify({ type: 'userkey' }));
  });

  // ── "the user clicked into this pane" signal ───────────────────────────────
  // This MUST come from here rather than from WPF. WebView2 is an HwndHost — a
  // native child window — and WPF routed mouse events (tunnelling Preview* ones
  // included) do not fire for input that lands on hosted native content. A
  // PreviewMouseLeftButtonDown on the host Border therefore only ever fires for
  // the 2px ring around the terminal, never for a click in the terminal itself,
  // so clicking a pane never made it the active session.
  //
  // fit() here as well: the grid rebuild that used to run on every activation
  // incidentally forced a layout pass and hence a re-fit. That rebuild is now
  // skipped when nothing visible changes, so re-fit on interaction has to be
  // explicit — otherwise xterm's column count can drift from what the PTY was
  // told, and redraws land a character off.
  var lastActivate = 0;
  document.addEventListener('mousedown', function () {
    var now = Date.now();
    if (now - lastActivate < 300) return;
    lastActivate = now;
    doFit();
    window.chrome.webview.postMessage(JSON.stringify({ type: 'activate' }));
  }, { capture: true });

  // ── Page-side diagnostics (issue #70) ──────────────────────────────────────
  // The host's timing ends at PostWebMessageAsString. If the renderer process is the
  // starved component — plausible at 25 panes, where 60+ WebView2 processes were measured
  // — every host-side number reads healthy while typing still stalls. These two probes
  // cover that blind spot. Off unless the host sends setDiag, and each reports only when
  // it crosses a threshold, so a healthy session produces no traffic at all.
  var diagOn = false;
  var lastPaintProbeMs = 0;

  function diagReport(what, ms, len) {
    try {
      window.chrome.webview.postMessage(JSON.stringify({
        type: 'diag', what: what, ms: ms, len: len
      }));
    } catch (e) {}
  }

  function diagWrite(data) {
    if (!diagOn) { term.write(data); return; }

    var t0 = performance.now();
    term.write(data);
    var t1 = performance.now();
    if (t1 - t0 > 50) diagReport('write-blocked', t1 - t0, data.length);

    // How long until the renderer actually produces a frame after this write. Sampled at
    // most once a second: an rAF per output chunk across every pane would itself be load,
    // and an instrument that changes the measurement is worth nothing here.
    if (t1 - lastPaintProbeMs > 1000) {
      lastPaintProbeMs = t1;
      requestAnimationFrame(function () {
        var lag = performance.now() - t1;
        if (lag > 100) diagReport('paint-lag', lag, data.length);
      });
    }
  }

  // ── Messages from WPF ──────────────────────────────────────────────────────
  window.chrome.webview.addEventListener('message', e => {
    try {
      const msg = JSON.parse(e.data);
      if      (msg.type === 'output')         diagWrite(msg.data);
      else if (msg.type === 'setDiag')        diagOn = !!msg.on;
      else if (msg.type === 'clear')          term.clear();
      else if (msg.type === 'focus')          { term.focus(); doFit(); }
      else if (msg.type === 'fit')            { doFit(); term.focus(); }
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
        doFit();
        // A profile override can switch fontFamily/fontSize, so the fit above measures the
        // old metrics. Re-fit on the next frame, once the new ones are in effect.
        //
        // Neither fonts API helps here. document.fonts.ready resolves once at page load
        // and stays resolved, so a .then() attached now runs synchronously with the stale
        // metrics — the bug this replaced. document.fonts.load() only matches
        // CSS-connected FontFace objects (@font-face rules); the families used here are
        // OS-installed and there are no such rules, so it resolves on the next microtask
        // having matched nothing. requestAnimationFrame is the honest signal: it fires
        // after the style change has been applied and measured.
        // Unconditional, and forced, so a font change that does not happen to alter the
        // column count is still reported rather than silently deduped away.
        //
        // The ack is posted separately and never skipped. doFit() declines to report an
        // unmeasurable pane (0x0 container, cell metrics not computed yet), and a host
        // waiting on this token would then have nothing to wait for but its own timeout —
        // 1.5s of dead launch per session. Splitting them keeps both properties: the host
        // only ever adopts a size it actually measured, and the wait always ends promptly.
        //
        // **The token is promoted HERE, not on arrival.** It used to be stamped at the top
        // of this handler, which handed the whole mechanism back its own bug: the doFit()
        // above — and any 'fit'/'focus' message landing before the frame — would post a
        // size measured with the OLD font carrying the NEW token, the host's
        // NoteOptionsToken would see an echo at least as new as it was waiting for, and
        // WaitForInitialSizeAsync would release on a pre-font measurement. That is the
        // exact failure the token exists to prevent. Promoting inside settle() means a
        // report can only carry the new token once the new metrics are the ones measured.
        // max() rather than assignment so two setOptions in flight (ApplyFontSettings then
        // ApplyProfileOverrides) can never walk the token backwards.
        //
        // **settle() must not depend on a frame.** WebView2 suspends rAF whenever the
        // control is not rendering — window minimized, or the wrapper detached by
        // RefreshTerminalLayout's TerminalGrid.Children.Clear() while a launch sits in its
        // await. Every other release path is gated off in that state (doFit declines an
        // unmeasurable pane, the ResizeObserver needs a size CHANGE, and the 50ms/250ms
        // one-shots were scheduled at page load and have long since fired), so nothing
        // acked and every affected launch burned the full 1.5s — ~37s across a 25-session
        // restore. The timer is the backstop; rAF still wins whenever frames are running,
        // so the measured-metrics path is unchanged in the normal case.
        const newToken = (typeof msg.token === 'number') ? msg.token : optionsToken;
        let settled = false;
        const settle = function () {
          if (settled) return;
          settled = true;
          if (newToken > optionsToken) optionsToken = newToken;
          doFit(true);
          try {
            window.chrome.webview.postMessage(JSON.stringify({
              type: 'optionsApplied', token: newToken
            }));
          } catch (e) {}
        };
        requestAnimationFrame(settle);
        setTimeout(settle, 250);
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
  // Wrapped, not passed by reference: ResizeObserver hands its callback the entries
  // array, which would arrive as doFit's truthy `force`.
  const resizeObserver = new ResizeObserver(function () { doFit(); });
  resizeObserver.observe(document.getElementById('terminal'));

  // Initial fit may have run while the WebView2 container was Collapsed (0×0).
  // Re-fit after a short delay so xterm picks up the real dimensions once visible.
  setTimeout(() => { doFit(); try { term.focus(); } catch {} }, 50);
  setTimeout(function () { doFit(); }, 250);

  // Re-fit once the font has actually loaded.
  //
  // xterm derives its column count from the MEASURED advance width of the font. The
  // first fit() runs immediately after term.open(); if Cascadia Code hasn't loaded
  // yet, xterm measures the fallback's metrics, computes the wrong cols, and reports
  // a width to the PTY that doesn't match what is drawn — text then wraps and
  // overlaps mid-line.
  //
  // The ResizeObserver above cannot correct this: the ELEMENT size never changed,
  // only the glyph metrics, so no resize fires. It stays wrong until something else
  // forces a fit, which is why switching layouts appeared to "fix" it.
  //
  // The two timeouts above are guesses at "fonts are probably ready by now" and are
  // easily too early during a heavy restore with many WebView2s initialising. They
  // stay as a fallback for the 0x0 case; this is the real signal.
  if (document.fonts && document.fonts.ready) {
    // Wrapped: .then() would pass the FontFaceSet as doFit's `force`.
    document.fonts.ready.then(function () { doFit(); });
  }

  term.focus();
