# Genshin Context

## Character Data

For characters, we need to read:
- element (enum) and name (str) from top left corner
- current level (1-90, 95, 100) and level cap (10,20,...,90,95,100) from 属性 panel
- constellation (0-6) from 命之座 panel
- A/E/Q talent levels (1-13) from 天赋 panel

We need to convert name to the character ids, element can be helpful for validation. (mappings.json will have element soon.)
Level cap is helpful to determine the ascension level.
Constellation is shown as 6 icons with labels, only activated icons have bright glow. If unsure, clicking the icon will also open details on the left where OCR can read "已激活" on already activated constellations (1-N), or a "激活" button on the next constellation to unlock (N+1), or a long text saying you have to unlock previous constellation first (N+2 or above).
The output talent levels need to be 1-10, we can strip out the +3 talent levels from the parsed constellation, with the data from mappings.json. In addition, tartaglia always has basic attack level +1, so we need to reduce his first talent level by 1 after scanning.
The constellation icons and talent levels have fixed position on the screen, and other text just vary slightly due to string length (semi-fixed positions).

## Weapon Data

For weapons, we need to read from the detail card on the right:
- name (str), possibly weapon type for validation
- rarity (3-5), star pixel detection to help with stopping condition (ignore 1-2 rarity weapons)
- level (1-90), level cap (10,20,...,90)
- refinement (1-5)
- lock status, pixel detection from lock icon
- equipping character (str)

Level cap is used to determine ascension level.
All weapon fields are displayed on fixed positions, except name for weapon and character have variable length.

## Artifact Data

For artifacts, we need to read from the detail card on the right:
- slot name (生之花、死之羽、时之沙、空之杯、理之冠)w
- main stat key
- rarity (4-5), star pixel detection to help with filtering stopping condition
- level (0-20 at rarity 5, 0-16 at rarity 4)
- lock status, pixel detection
- astral mark, pixel detection
- elixir crafted, can also be pixel detection
- sub stat keys and values (2-4 pairs), including unactivated stat (rarity 5 only)
- set name (str)
- equipping character (str)

We should only keep artifacts that are at their max rarity (skip rarity 4 versions of artifacts that has max rarity of 5, unless equipped by some characters).
Elixir crafted artifacts have a purple banner that pushes level/lock/astral/substats/setname down. The other fields like slot name, main stat key, rarity star, equipping character have fixed positions.
Rarity 4 artifacts can have a variable number of stats, from 2 to 4. Rarity 5 artifacts always have 4 stat rows, however the last one can be gray with an additiona text "(待激活)".

The GOOD v3 format requires us to know the total rolls, and the rolls per stat, and the initial value of a stat. We can only infer the number of rolls from the numbers we got. If we don't yet have algorithm to figure them out, it's also okay to have inaccuracies in those fields for now. We do not have the initial value of stats from OCR.