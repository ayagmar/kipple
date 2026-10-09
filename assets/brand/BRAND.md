# kipple brand

## Kip

Kip is a small sea otter who keeps one glowing amber pebble, the thing worth keeping, and tosses the clutter. It is the "keep what matters" idea in kipple (`p` pins an item). Kip is calm and warm, and never mocks the user's data.

## Files

| File | Use |
| --- | --- |
| `logo.svg`, `logo-dark.svg` | Kip holding the pebble. README, site header, docs, anything 48 px and up |
| `mark.svg`, `mark-dark.svg` | Kip's head only. Small sizes, avatars, terminal-adjacent places |
| `favicon.svg`, `favicon.ico`, `icon-512.png` | Browser tab and app icon (head only, heavier outline) |
| `wordmark.svg`, `wordmark-dark.svg` | "kipple" on its own |
| `lockup.svg`, `lockup-dark.svg` | Logo plus wordmark, side by side |
| `banner.svg`, `banner-dark.svg` | README header (switched with `<picture>`) |
| `social-card.png` (`.svg` source) | GitHub social preview, 1280×640. Upload it in repo Settings → Social preview |

`-dark` variants are for dark backgrounds. They swap the near-black outline for a warm brown so the silhouette stays visible.

## Colour

| Name | Hex | Use |
| --- | --- | --- |
| ink | `#15171C` | text, dark backgrounds, eyes and nose |
| outline | `#20232C` | outlines on light backgrounds |
| outline dark | `#4A2A1C` | outlines on dark backgrounds |
| bone | `#F4EADE` | Kip's face, light text on dark |
| paper | `#F4EFE6` | light backgrounds |
| fur | `#CA8848` | Kip |
| amber | `#F2A23A` | the pebble, and "what matters / what's kept" in the UI. Use it for nothing else |

## Type

The wordmark is Fredoka SemiBold (SIL Open Font License 1.1, by The Fredoka Project Authors), converted to outlines. The tagline uses Fredoka Regular. The site picks its own type in `DESIGN.md` (M6) and keeps Fredoka for the wordmark.

## Rules

- Write `kipple` in lowercase, always.
- Clear space around the logo is at least half the logo's height. Minimum size: 16 px for `mark`/`favicon`, 48 px for `logo`.
- Don't recolour Kip, don't add effects or shadows, and don't stretch, rotate or crop the logo.
- Don't put the pebble glow on busy backgrounds. Use `paper` or `ink`.
- Kip doesn't appear in error messages about deletion failures. Errors stay plain.
