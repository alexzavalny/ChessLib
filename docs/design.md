# ChessLib design reference

Use the [ChessLib icon](../macos/AppIcon.png) as the visual reference for the redesign. Its main materials are forest-green bookcloth, warm ivory paper, and a small burnt-orange ribbon. Carry the fine grain and shallow relief into surfaces that benefit from texture, while keeping the interface quiet enough for reading moves and notes.

## Icon palette

These are exact sRGB samples from the original 1254 × 1254 PNG. Coordinates are zero-based from the top left. The hex and RGB values are the source color channels before alpha compositing. Each swatch represents one spot in a textured, lit surface.

| Icon detail | Hex | RGB | Pixel (x, y) | Suggested use |
| --- | --- | --- | --- | --- |
| Forest-green bookcloth | `#173E21` | 23, 62, 33 | (400, 130) | Primary brand green for the header or other compact, dark surfaces |
| Lighter bookcloth threads | `#254830` | 37, 72, 48 | (233, 588) | Subtle green-on-green surface variation |
| Deep cloth shadow | `#05230E` | 5, 35, 14 | (500, 1150) | Recesses and small edge shadows |
| Light ivory paper | `#F7ECDD` | 247, 236, 221 | (390, 780) | Reference for warm, light work surfaces |
| Carved ivory | `#F5E9D6` | 245, 233, 214 | (560, 389) | Secondary ivory for small panels and illustrated details |
| Paper fold shadow | `#CBB99B` | 203, 185, 155 | (465, 905) | Warm dividers and shallow relief on ivory |
| Deep paper bevel | `#BDA581` | 189, 165, 129 | (725, 935) | Small inset edges and deeper material shadows |
| Burnt-orange ribbon | `#CD4C05` | 205, 76, 5 | (603, 1040) | A sparing accent for a bookmark, favourite, or primary action |
| Ribbon highlight | `#EF7421` | 239, 116, 33 | (612, 1005) | Highlight for small orange details |

## Texture and depth

| Material | What to carry forward |
| --- | --- |
| **Bookcloth** | A dense, matte weave with tiny, low-contrast threads. At normal window size it should read as fine grain. Use it on the board and selectively on broad green surfaces. |
| **Ivory paper** | A warm, softly fibrous surface with very little contrast. Use smooth ivory for reading areas and add paper grain only where there is room to see it. |
| **Ribbon** | A slightly more defined fabric weave and a warm edge highlight. Keep it to small accents so it retains the feel of a physical bookmark. |
| **Carved stone** | The existing [chess pieces](../assets/stone-pieces.png) extend the icon's sculptural treatment with ivory marble and charcoal basalt. Keep the stone grain inside the silhouettes and preserve the simple Greek-inspired forms. |

Use broad, soft light and short contact shadows. Let fine grain and a shallow bevel suggest the material. Keep text, move notation, and controls on calm surfaces with clear contrast.

## Board reference

The board uses lighter tints for playability. The current tint inputs are very light beige `#FFFAE7` and muted green `#719C7A`. The [bookcloth texture](../assets/bookcloth.png) is applied over both colors and repeated four times across each board axis. Keep the weave this fine and check it at the default and smallest supported window sizes. Check both piece colors on both squares and keep move highlights easy to spot.
