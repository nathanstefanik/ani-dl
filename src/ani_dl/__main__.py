"""Entry point: ``python -m ani_dl`` and the ``ani-dl`` console script."""

import sys

from .cli import main


def main_entry() -> None:  # console_scripts target
    raise SystemExit(main())


if __name__ == "__main__":
    sys.exit(main())
