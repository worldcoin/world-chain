#!/usr/bin/env python3
"""
mdbook preprocessor: strip YAML front matter from chapter content.

YAML front matter is a block delimited by '---' at the very start of the file.
mdbook v0.5+ protocol: stdin receives [context, book]; stdout must be just the book object.
"""
import json
import sys
import re

FRONT_MATTER_RE = re.compile(r'^\s*---\s*\n.*?\n---\s*\n', re.DOTALL)


def strip_front_matter(content):
    """Remove YAML front matter (---...---) from the start of content."""
    return FRONT_MATTER_RE.sub('', content, count=1)


def process_items(items):
    for item in items:
        if 'Chapter' in item:
            chapter = item['Chapter']
            if chapter.get('content'):
                chapter['content'] = strip_front_matter(chapter['content'])
            if chapter.get('sub_items'):
                process_items(chapter['sub_items'])


def main():
    if len(sys.argv) > 1 and sys.argv[1] == 'supports':
        sys.exit(0)

    raw = sys.stdin.read()
    data = json.loads(raw)
    # mdbook v0.5: sends [context, book]; expects just the book object back
    _context, book = data
    process_items(book['items'])
    print(json.dumps(book))


if __name__ == '__main__':
    main()
