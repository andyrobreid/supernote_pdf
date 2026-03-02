# Change Request

Make sure not to commit this file and don't apply to gitignore. This is a working document that should always be located here but never committed

I've noticed when using the new `--pdf-and-markdown` flag the markdown is not formatted much and also duplicates what I'd need. Let's try and resolve this.

## Template

Here is a markdown block example of the current format. You'll get this if you investigate how the `../supernote-companion` project integrates with the result of this project.

```md
---
name: 2025 - LBC - Galatians Colossians study
supernote_id: sn-4edfd817
source: /Note/2025 - LBC - Galatians Colossians study.note
created: 2025-12-07
modified: 2025-12-07
size: 71.3 MB
pdf_attachment: REFERENCE/SUPERNOTE/PDF's/2025 - LBC - Galatians Colossians study.pdf
tags:
  - supernote
updated: 2026-03-01T22:07
---

# 2025 - LBC - Galatians Colossians study

## Note Information

| Property | Value |
|----------|-------|
| **Source** | `/Note/2025 - LBC - Galatians Colossians study.note` |
| **Created** | December 7, 2025 at 08:07 AM |
| **Modified** | December 7, 2025 at 08:07 AM |
| **Size** | 71.3 MB |

## PDF Attachment

![[REFERENCE/SUPERNOTE/PDF's/2025 - LBC - Galatians Colossians study.pdf]]

---

## Notes

*Add your notes and annotations here...*
```

## Solution

I'd like to extend upon this template that the markdown file produced adds the text in a section after the referenced pdf attachment titled `Text`.
Please meet all the meta data provided the numbers in this template were derived from the `.note` file.
This is used within obsidian and I use wikilink style links.
The `updated` and `created` fields are created from another plugin that you don't need to produce, this will be handled by another plugin. You can however have equivalent fields prefixed with `supernote_` that are from the super note .note file metadata
The template above is produced from `2025 - LBC - Galatians Colossians study.note` file.
I've noticed with the files provided using the flag `--pdf-and-markdown` have like 2 sections of text. Text like its written then the same text but a newline for each word. Can I remove the latter section. I'd rather just have the text in paragraph form.

Now make it an option with another flag to remove the new lines altogether and only have spaces between words. Unless there is 2 new line characters detected. Then this would leave the text nicer formatted for editors.
