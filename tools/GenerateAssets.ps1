# GenerateAssets.ps1 — generates Assets/app.ico and installer/banner.bmp
# Run from repo root: pwsh -File tools/GenerateAssets.ps1

param(
    [string]$RepoRoot = (Split-Path $PSScriptRoot -Parent)
)

Add-Type -AssemblyName System.Drawing

# ── Helper: save bitmap as multi-size ICO ────────────────────────────────────
function Save-Ico {
    param([System.Drawing.Bitmap[]]$Bitmaps, [string]$OutPath)

    $ms = [System.IO.MemoryStream]::new()
    $bw = [System.IO.BinaryWriter]::new($ms)

    $count = $Bitmaps.Length
    # ICONDIR header
    $bw.Write([uint16]0)      # reserved
    $bw.Write([uint16]1)      # type = ICO
    $bw.Write([uint16]$count) # image count

    $pngDatas = @()
    foreach ($bmp in $Bitmaps) {
        $pngMs = [System.IO.MemoryStream]::new()
        $bmp.Save($pngMs, [System.Drawing.Imaging.ImageFormat]::Png)
        $pngDatas += ,$pngMs.ToArray()
        $pngMs.Dispose()
    }

    # Each ICONDIRENTRY is 16 bytes
    $offset = 6 + 16 * $count
    for ($i = 0; $i -lt $count; $i++) {
        $sz   = $Bitmaps[$i].Width
        $data = $pngDatas[$i]
        $bw.Write([byte]($sz -lt 256 ? $sz : 0))  # width (0 = 256)
        $bw.Write([byte]($sz -lt 256 ? $sz : 0))  # height
        $bw.Write([byte]0)    # color count
        $bw.Write([byte]0)    # reserved
        $bw.Write([uint16]1)  # color planes
        $bw.Write([uint16]32) # bits per pixel
        $bw.Write([uint32]$data.Length)
        $bw.Write([uint32]$offset)
        $offset += $data.Length
    }
    foreach ($data in $pngDatas) { $bw.Write($data) }
    $bw.Flush()

    [System.IO.File]::WriteAllBytes($OutPath, $ms.ToArray())
    $ms.Dispose()
    Write-Host "  Saved ICO: $OutPath"
}

# ── Draw terminal icon on a bitmap ───────────────────────────────────────────
function New-TerminalBitmap {
    param([int]$Size)

    $bmp = [System.Drawing.Bitmap]::new($Size, $Size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit

    # Background: dark rounded rect
    $bg = [System.Drawing.Color]::FromArgb(255, 30, 30, 46)  # #1e1e2e
    $g.Clear([System.Drawing.Color]::Transparent)

    $radius = [int]($Size * 0.18)
    $rect = [System.Drawing.Rectangle]::new(0, 0, $Size, $Size)
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $path.AddArc($rect.X, $rect.Y, $radius*2, $radius*2, 180, 90)
    $path.AddArc($rect.Right - $radius*2, $rect.Y, $radius*2, $radius*2, 270, 90)
    $path.AddArc($rect.Right - $radius*2, $rect.Bottom - $radius*2, $radius*2, $radius*2, 0, 90)
    $path.AddArc($rect.X, $rect.Bottom - $radius*2, $radius*2, $radius*2, 90, 90)
    $path.CloseFigure()
    $g.FillPath([System.Drawing.SolidBrush]::new($bg), $path)

    # Prompt text ">_" in accent blue
    $accent = [System.Drawing.Color]::FromArgb(255, 137, 180, 250)  # #89b4fa
    $fontSize = [float]($Size * 0.38)
    $font = [System.Drawing.Font]::new("Consolas", $fontSize, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $brush = [System.Drawing.SolidBrush]::new($accent)
    $text = ">_"
    $sf = [System.Drawing.StringFormat]::new()
    $sf.Alignment = [System.Drawing.StringAlignment]::Center
    $sf.LineAlignment = [System.Drawing.StringAlignment]::Center
    $rf = [System.Drawing.RectangleF]::new(0, 0, $Size, $Size)
    $g.DrawString($text, $font, $brush, $rf, $sf)

    $g.Dispose()
    $font.Dispose()
    return $bmp
}

# ── Generate ICO ─────────────────────────────────────────────────────────────
Write-Host "Generating app.ico..."
$assetsDir = Join-Path $RepoRoot "src\CodeShellManager\Assets"
New-Item -ItemType Directory -Force $assetsDir | Out-Null

$sizes = @(16, 24, 32, 48, 64, 128, 256)
$bitmaps = $sizes | ForEach-Object { New-TerminalBitmap -Size $_ }
Save-Ico -Bitmaps $bitmaps -OutPath (Join-Path $assetsDir "app.ico")
foreach ($b in $bitmaps) { $b.Dispose() }

# ── Generate installer banner.bmp (493 x 58) ─────────────────────────────────
Write-Host "Generating installer/banner.bmp..."
$installerDir = Join-Path $RepoRoot "installer"
New-Item -ItemType Directory -Force $installerDir | Out-Null

$bmp = [System.Drawing.Bitmap]::new(493, 58)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit

# Background gradient: dark
$bgColor  = [System.Drawing.Color]::FromArgb(255, 24, 24, 37)   # #181825
$bg2Color = [System.Drawing.Color]::FromArgb(255, 30, 30, 46)   # #1e1e2e
$grad = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
    [System.Drawing.Point]::new(0, 0),
    [System.Drawing.Point]::new(493, 0),
    $bgColor, $bg2Color)
$g.FillRectangle($grad, 0, 0, 493, 58)

# Left accent stripe (blue)
$stripe = [System.Drawing.Color]::FromArgb(255, 137, 180, 250)  # #89b4fa
$g.FillRectangle([System.Drawing.SolidBrush]::new($stripe), 0, 0, 4, 58)

# Icon text ">_"
$iconFont = [System.Drawing.Font]::new("Consolas", 22, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
$iconBrush = [System.Drawing.SolidBrush]::new($stripe)
$g.DrawString(">_", $iconFont, $iconBrush, [System.Drawing.PointF]::new(14, 12))

# App name
$titleFont = [System.Drawing.Font]::new("Segoe UI", 18, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
$fgBrush   = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 205, 214, 244))  # #cdd6f4
$g.DrawString("CodeShellManager", $titleFont, $fgBrush, [System.Drawing.PointF]::new(54, 10))

# Sub-text
$subFont  = [System.Drawing.Font]::new("Segoe UI", 11, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
$mutedBrush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 108, 112, 134))  # #6c7086
$g.DrawString("by umage.ai", $subFont, $mutedBrush, [System.Drawing.PointF]::new(56, 34))

$g.Dispose()
$bmp.Save((Join-Path $installerDir "banner.bmp"), [System.Drawing.Imaging.ImageFormat]::Bmp)
$bmp.Dispose()
Write-Host "  Saved BMP: installer/banner.bmp"

Write-Host "Done."
