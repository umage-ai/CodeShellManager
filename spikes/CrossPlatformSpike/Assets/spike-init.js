const term = new Terminal({
  cursorBlink: true,
  fontSize: 14,
  fontFamily: "'Cascadia Code', Menlo, 'DejaVu Sans Mono', Consolas, monospace",
  theme: { background: '#1e1e2e', foreground: '#cdd6f4', cursor: '#cdd6f4' },
  scrollback: 5000,
  allowProposedApi: true,
  customGlyphs: true,
});

const fitAddon = new FitAddon.FitAddon();
term.loadAddon(fitAddon);
term.open(document.getElementById('terminal'));
fitAddon.fit();

// invokeCSharpAction is injected by Avalonia's NativeWebView and raises
// WebMessageReceived on the C# side. Guarded so the page also works when
// opened in a plain browser for debugging.
function send(msg) {
  if (window.invokeCSharpAction) window.invokeCSharpAction(JSON.stringify(msg));
}

// C# pushes PTY output here as base64-encoded raw bytes. xterm.js accepts a
// Uint8Array and does its own UTF-8 decoding, so multi-byte characters split
// across chunk boundaries render correctly.
window.ptyData = (b64) => {
  const raw = atob(b64);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  term.write(bytes);
};

term.onData(data => send({ type: 'input', data }));
term.onResize(({ cols, rows }) => send({ type: 'resize', cols, rows }));

new ResizeObserver(() => { try { fitAddon.fit(); } catch {} })
  .observe(document.getElementById('terminal'));

send({ type: 'ready', cols: term.cols, rows: term.rows });
term.focus();
