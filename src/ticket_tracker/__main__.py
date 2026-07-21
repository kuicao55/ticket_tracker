"""让 `python -m ticket_tracker` 等同于 `tt`。"""

from ticket_tracker.cli import main

if __name__ == "__main__":
    main()
