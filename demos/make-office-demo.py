#!/usr/bin/env python3
"""Create a small, deterministic Word document with a real 2x2 table."""
import sys
from pathlib import Path
from xml.sax.saxutils import escape
from zipfile import ZipFile, ZIP_DEFLATED

def paragraph(text):
    return f'<w:p><w:r><w:t xml:space="preserve">{escape(text)}</w:t></w:r></w:p>'

def make(path):
    rows = []
    for values in [("担当", "作業内容"), ("Alice / Bob", "ここも同時に編集できます")]:
        cells = ''.join('<w:tc><w:tcPr><w:tcW w:w="4300" w:type="dxa"/></w:tcPr>' + paragraph(v) + '</w:tc>' for v in values)
        rows.append('<w:tr>' + cells + '</w:tr>')
    table = '<w:tbl><w:tblPr><w:tblBorders>' + ''.join(f'<w:{k} w:val="single" w:sz="4" w:color="CCCCDD"/>' for k in ['top','left','bottom','right','insideH','insideV']) + '</w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="4300"/><w:gridCol w:w="4300"/></w:tblGrid>' + ''.join(rows) + '</w:tbl>'
    body = ''.join(map(paragraph, ["jade office — 共同編集", "AliceとBobで、この段落を同時に編集してみましょう。", "日本語と絵文字も使えます 😀"])) + table + paragraph("変更はDOCXに保存されます。")
    document = '<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>' + body + '<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1100" w:right="1100" w:bottom="1100" w:left="1100"/></w:sectPr></w:body></w:document>'
    content_types = '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'
    rels = '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>'
    with ZipFile(path,'w',ZIP_DEFLATED) as z:
        for name, value in [('word/document.xml',document),('[Content_Types].xml',content_types),('_rels/.rels',rels)]:
            # Fixed ZIP time makes the demo seed reproducible.
            from zipfile import ZipInfo
            info = ZipInfo(name,(2026,1,1,0,0,0)); info.compress_type = ZIP_DEFLATED
            z.writestr(info,value)

if __name__ == '__main__':
    output = Path(sys.argv[1]); output.parent.mkdir(parents=True,exist_ok=True); make(output)
