# Agent download workflow

When the user asks to download anime, run `ani-dl` in one shot. Flag meanings live in `ani-dl --help`.

## Download

1. Map title, season, episode span, quality, and directory onto flags.
   Done when every named constraint is either a flag or part of the query string.
2. Run one command of the form:

   ```sh
   ani-dl -q <quality> -e <range> -d <dir> "<query>"
   ```

   Done when that process is running or has exited. Passing `-e` skips the TUI and picks search result 1.
3. If the downloaded title is the wrong show, re-run with `-n <N>` (1-based).
   Done when the selected title matches the request.

## Range, path, season

- **Til end:** `-e 5--1`. Whole show is `0--1`; latest only is `-1`. Use the `-1` sentinel rather than a looked-up last-episode number.
- **Directory:** pass `-d` whenever the user names a path. Default is the current directory.
- **Season:** put it in the query (`"classroom of the elite season 4"`). `-s` only changes the `Sxx` filename tag.
- **Dubbed:** `-D`. Default is subbed.
- **Quality:** `best` | `worst` | `1080` | `720` | `480`. Default `best`.

## Example

User: download classroom of the elite season 4 episodes 5 til end at best quality in `~/downloads/videos`

```sh
ani-dl -q best -e 5--1 -d ~/downloads/videos "classroom of the elite season 4"
```
