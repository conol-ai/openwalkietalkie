# SPDX-License-Identifier: MIT
# Regenerate:
#   uv run --with openpyxl --with xlrd python hardware/rev-b/scripts/tooling/build_fab_quote_zh.py \
#       [--fab ref/fab] [--out output/OpenWalkieTalkie-RevB-quote-YYYYMMDD]
#
# Build the Chinese quotation package for a PCB/PCBA fab other than JLCPCB:
#   - a cascaded BOM of the two board assemblies that lists the parts JLCPCB
#     actually placed (read from JLC's "BOM对比结果" exports, main.xls and
#     encoder.xls) next to the design parts,
#   - the Rev B main board and the encoder-wheel board Gerber zips,
#   - the pick-and-place files and the pin-1 check lists,
#   - a README with the PCB and assembly requirements.
# JLC's export strips the spaces from the customer columns, so design data
# comes from the repository's BOM CSVs and only the placed-part columns come
# from the export.
import argparse, csv, datetime, re, shutil, sys, zipfile
from pathlib import Path

import xlrd
from openpyxl import Workbook
from openpyxl.styles import Alignment, Border, Font, PatternFill, Side
from openpyxl.utils import get_column_letter
from openpyxl.worksheet.properties import Outline

REPO = Path(__file__).resolve().parents[4]
REVB = REPO / "hardware/rev-b"
R6 = REPO / "hardware/rev-a/pcb-release-bodyv6.13-20260923-r6"
ENC = R6 / "fab-jlcpcb/encoder-wheel"

ap = argparse.ArgumentParser()
ap.add_argument("--fab", default=str(REPO / "ref/fab"))
ap.add_argument("--out", default="")
ap.add_argument("--date", default=datetime.date.today().isoformat())
args = ap.parse_args()
OUT = Path(args.out or REPO / f"output/OpenWalkieTalkie-RevB-quote-{args.date.replace('-', '')}")

# ---------------------------------------------------------------- inputs
def norm(s):
    return re.sub(r"[^A-Z0-9]", "", str(s).upper())

def refs_of(s):
    return [d.strip() for d in str(s).split(",") if d.strip()]

def read_design(path):
    with open(path, newline="", encoding="utf-8") as f:
        return {frozenset(refs_of(r["Designator"])): r for r in csv.DictReader(f)}

def read_jlc(path):
    """Rows of JLC's comparison export, keyed by designator set. Rows that
    share a part with an earlier row leave the part columns empty; they
    inherit them."""
    sh = xlrd.open_workbook(path).sheet_by_index(0)
    head = [str(c.value) for c in sh.row(2)]
    col = {h: i for i, h in enumerate(head)}
    out, by_part = {}, {}
    for r in range(3, sh.nrows):
        v = [c.value for c in sh.row(r)]
        g = lambda h: str(v[col[h]]).strip()
        part = g("元件编号")
        d = dict(part=part, name=g("元件名称"), mpn=g("原厂型号"), pkg=g("封装规格"), brand=g("品牌"), cat=g("类别"),
                 lib=g("库类型"), source=g("元器件来源"), match=g("匹配方式"),
                 top=int(float(g("顶层数") or 0)), bottom=int(float(g("底层数") or 0)),
                 design_part=g("LCSCPart#"))
        if d["mpn"]:
            by_part[part] = d
        else:
            d.update({k: by_part[part][k] for k in ("name", "mpn", "pkg", "brand", "cat", "lib", "source")})
        out[frozenset(refs_of(v[col["Designator"]]))] = d
    return out

design_main = read_design(REVB / "fab-jlcpcb/main-bom-jlcpcb.csv")
design_enc = read_design(ENC / "encoder-wheel-bom-jlcpcb.csv")
jlc_main = read_jlc(Path(args.fab) / "main.xls")
jlc_enc = read_jlc(Path(args.fab) / "encoder.xls")
for name, a, b in (("main", design_main, jlc_main), ("encoder", design_enc, jlc_enc)):
    if set(a) != set(b):
        sys.exit(f"{name}: designators differ between the repository BOM and JLC's export: "
                 f"{sorted(map(sorted, set(a) ^ set(b)))}")
if sum(d["bottom"] for d in jlc_enc.values()) != 1:
    sys.exit("encoder.xls is not the encoder-wheel board (ENC1 on the bottom)")

# ---------------------------------------------------------------- wording
BRAND = {"UNI-ROYAL(厚声)": "厚声 UNI-ROYAL", "SAMSUNG(三星)": "三星 Samsung", "YAGEO(国巨)": "国巨 Yageo",
         "ST(意法半导体)": "意法半导体 ST", "TI(德州仪器)": "德州仪器 TI", "DIANWEI(钿威科技)": "钿威 DIANWEI",
         "GigaDevice(兆易创新)": "兆易创新 GigaDevice", "Sunlord(顺络)": "顺络 Sunlord", "Nsiway(纳芯威)": "纳芯威 Nsiway",
         "FOJAN(富捷)": "富捷 FOJAN", "HRE(芯声)": "芯声 HRE", "hanxia(韩下)": "韩下 hanxia", "HRS(广濑)": "广濑 Hirose",
         "TKD(泰晶)": "泰晶 TKD", "FH(风华)": "风华 FH", "muRata(村田)": "村田 Murata", "RICHTEK(立锜)": "立锜 Richtek",
         "ALPSALPINE(阿尔卑斯阿尔派)": "阿尔卑斯阿尔派 Alps Alpine", "韩国韩荣": "韩荣 HRO", "MOLEX": "Molex",
         "ABRACON": "Abracon", "思澈科技": "思澈 SiFli"}
DESIGN_MFR = {"Samsung Electro-Mechanics": "Samsung", "FH (Guangdong Fenghua)": "风华 FH", "Texas Instruments": "TI",
              "STMicroelectronics": "ST", "SiFli Technologies": "SiFli", "Johanson Technology": "Johanson"}

# first designator -> (name, description, substitution rule)
KEEP = "指定型号，不可替代；缺货请先与我方确认"
SPECIAL = {
    "AE1": ("2.4 GHz 陶瓷贴片天线", "蓝牙天线，1206（3.2 × 1.6 mm）",
            "射频关键件。按实际贴装型号或设计型号供料，其他型号需我方确认（匹配网络需重调）"),
    "J1": ("扬声器连接器 1.25 mm 2P", "Molex PicoBlade 卧式贴片针座，2 位，间距 1.25 mm", KEEP),
    "J2": ("板间 FPC 连接器 0.5 mm 4P", "翻盖式 FPC 连接器，4 位，间距 0.5 mm，卧贴；接编码器板", "(50) 卷装与 (99) 小包装为同一连接器，均可"),
    "J3": ("电池连接器 1.25 mm 3P", "Molex PicoBlade 卧式贴片针座，3 位，间距 1.25 mm；1 VCELL、2 NTC、3 GND", KEEP),
    "J5": ("USB Type-C 母座 16P", "卧贴，外壳带插件定位脚（需过孔焊接）", KEEP),
    "L1": ("铁氧体磁珠 600 Ω", "600 Ω @100 MHz，1.5 A，1206；扬声器输出", KEEP),
    "L3": ("功率电感 4.7 µH", "4.7 µH ±20%，2016（0806）", KEEP),
    "L4": ("功率电感 2.2 µH", "2.2 µH ±20%，2.4 A，2016（0806）", KEEP),
    "MK1": ("PDM 数字麦克风", "MEMS 数字麦克风，HCLGA 3 × 4 mm，顶部进音；贴于底面", KEEP + "。回流后不可水洗、不可堵塞进音孔"),
    "SW1": ("侧按轻触开关", "3.9 × 2.9 × 1.55 mm，侧向按压，贴片；ENTER / ESC / PTT / SHIFT", KEEP),
    "SW3": ("轻触开关 6 × 3.6 × 2.5 mm", "立贴轻触开关，6 × 3.6 mm，高 2.5 mm，按力约 2.5 N；前面左/右键",
            "实际贴装型号或设计型号均可；其他型号须封装、高度 2.5 mm 一致"),
    "U1": ("音频功率放大器", "3 W 单声道 D 类功放，MSOP-8", KEEP),
    "U2": ("负载开关", "5.5 V / 2 A 负载开关，SOT-23-6", KEEP),
    "U3": ("显示/触摸 FPC 连接器 0.3 mm 31P", "翻盖式 FPC 连接器，31 位，间距 0.3 mm，交错脚，卧贴；贴于底面", KEEP),
    "U4": ("SPI NOR Flash 128 Mbit", "128 Mbit QSPI NOR Flash，3.3 V，SOP-8 208 mil",
           "按实际贴装型号或设计型号供料，其他型号需我方确认（固件需适配）"),
    "U5": ("主控 MCU（蓝牙 SoC）", "SiFli SF32LB525UC6，QFN-68（7 × 7 mm，带散热焊盘）；贴于底面", KEEP + "。上批为我方提供（客供料）"),
    "U6": ("锂电池充电与电源路径管理 IC", "单节锂电线性充电 + 电源路径，VQFN-16（3 × 3 mm，带散热焊盘）", KEEP),
    "U7": ("升降压 DC-DC", "同步升降压，800 mA，1.8–5.5 V，WDFN-10（2.5 × 2.5 mm，带散热焊盘）", KEEP),
    "U20": ("USB ESD 保护", "USB 数据线 ESD 保护阵列，SOT-23-6", KEEP),
    "Y1": ("无源晶振 48 MHz", "48 MHz，2016 四脚贴片，CL 8 pF，±10 ppm；要求 CL 7–12 pF、ESR ≤ 30 Ω",
           "按实际贴装型号或设计型号供料；其他型号须满足 CL 7–12 pF、ESR ≤ 30 Ω 并经我方确认"),
    "ENC1": ("旋转编码器（拨轮）", "增量式贯通轴编码器，贴片；贴于编码器板底面（拨轮内）", KEEP),
}
SPECIAL_ENC = {"J1": ("板间 FPC 连接器 0.5 mm 4P", "翻盖式 FPC 连接器，4 位，间距 0.5 mm，卧贴；接主板 J2", SPECIAL["J2"][2]),
               "ENC1": SPECIAL["ENC1"]}
PKG = {"CHIP_0402": "0402", "CHIP_0603": "0603", "CHIP_0805": "0805", "CHIP_1206": "1206"}

def dielectric(mpn):
    m = mpn.upper()
    if "NPO" in m or "C0G" in m or m.startswith("0402CG"):
        return "C0G"
    for d in ("X5R", "X7R"):
        if d in m:
            return d
    if m.startswith("CL"):                       # Samsung: 5th character A = X5R, B = X7R
        return {"A": "X5R", "B": "X7R"}.get(m[4], "")
    return ""

def describe(first, d, j, special):
    if first in special:
        return special[first]
    pkg = PKG.get(d["Footprint"], j["pkg"])
    if first.startswith("C"):
        die = dielectric(d["Design MPN"])
        spec = j["name"].replace("uF", " µF").replace("nF", " nF").replace("pF", " pF")
        rule = ("必须 C0G/NP0，容值、精度、耐压不低于所列；可用同规格品牌料" if die == "C0G"
                else "可用同规格品牌料：容值、精度相同，介质 X5R/X7R，耐压不低于所列")
        return f"贴片电容 {d['Comment'].replace('uF', ' µF').replace('nF', ' nF').replace('pF', ' pF')} {pkg}", f"{spec}，{die}，{pkg}", rule
    if first.startswith("R"):
        val = d["Comment"].replace("ohm", "Ω").replace("MΩ", " MΩ").replace("kΩ", " kΩ")
        val = re.sub(r"(?<=\d)Ω", " Ω", val)
        return f"贴片电阻 {val} {pkg}", f"{j['name']}，{pkg}", "可用同规格品牌料：阻值相同，精度 ±1%，封装相同"
    return d["Comment"], j["name"], KEEP

def status(d, j):
    """How the placed part relates to the design part."""
    if norm(j["mpn"]) == norm(d["Design MPN"]):
        return "与设计一致"
    planned = d["Part placed (if substituted)"]
    if planned and norm(j["mpn"]) in norm(planned):
        return "预定替代"
    return "库存改料"

REASON = {  # first designator -> remark for parts the fab changed
    "C42": "上批因库存改为三星同规格料（10 µF 16 V X7R 1206）。",
    "U4": "上批因库存改为兆易 GD25Q128ESIGR（同容量、同封装 QSPI NOR Flash）。",
    "R37": "上批因库存改为富捷同规格料（3.6 kΩ ±1% 0402）。设定充电电流 890 / 3.6 k ≈ 247 mA，阻值不可改。",
    "AE1": "上批因库存改为 Abracon AMCA31-2R450G-S1F-T3（同为 2.45 GHz 1206 陶瓷天线）。",
    "C7": "上批因库存改为芯声同规格料（100 nF 50 V C0G 1206）。音频通路，必须 C0G。",
    "SW3": "上批因库存改为韩下同尺寸轻触开关。",
    "Y1": "设计型号立创无货，上批用泰晶 SX20Y048000B81T001。",
}
EXTRA = {
    "U5": "上批由我方邮寄提供（客供料）。请说明可否代采，或继续按客供料报价。",
    "R32": "上批使用我方在嘉立创的闲置库存。",
    "J5": "外壳定位脚为通孔，需过孔焊接；连接器伸出板边。",
    "SW1": "SW1 / SW2 / SW5 / SW6 伸出板边。",
    "MK1": "进音孔朝外（背离 PCB），对准外壳声孔。",
    "R40": "天线匹配位（S1 / S2），暂贴 0 Ω，后续可能按网分测试结果改值。",
    "C57": "射频隔直电容，必须 C0G。",
    "J3": "配对插头 Molex 51021-0300。",
    "J1": "配对插头 Molex 51021-0200。",
}

# ---------------------------------------------------------------- rows
rows = []
def add(parent, **kw):
    r = dict(parent=parent, name="", spec="", refs="", qty=1, unit="个", pkg="", side="", brand="", mpn="", part="",
             dbrand="", dmpn="", status="", rule="", note="")
    r.update(kw)
    rows.append(r)
    return len(rows) - 1

def ref_key(k):
    first = sorted(k, key=lambda s: (re.sub(r"\d", "", s), int(re.sub(r"\D", "", s))))[0]
    return (re.sub(r"\d", "", first), int(re.sub(r"\D", "", first)))

def add_parts(parent, design, jlc, special, extra):
    n = 0
    for k in sorted(design, key=ref_key):
        d, j = design[k], jlc[k]
        refs = sorted(k, key=lambda s: int(re.sub(r"\D", "", s)))
        first = refs[0]
        if int(d["Quantity"]) != len(refs) or j["top"] + j["bottom"] != len(refs):
            sys.exit(f"{first}: quantity mismatch")
        name, spec, rule = describe(first, d, j, special)
        st = status(d, j)
        note = " ".join(x for x in ((REASON.get(first, "") if st == "库存改料" else ""), extra.get(first, "")) if x)
        side = "，".join(x for x in (f"顶面 {j['top']}" if j["top"] else "", f"底面 {j['bottom']}" if j["bottom"] else "") if x)
        add(parent, name=name, spec=spec, refs=", ".join(refs), qty=len(refs), pkg=j["pkg"], side=side,
            brand=BRAND.get(j["brand"], j["brand"]), mpn=j["mpn"], part=j["part"],
            dbrand=DESIGN_MFR.get(d["Design Manufacturer"], d["Design Manufacturer"]), dmpn=d["Design MPN"],
            status=st, rule=rule, note=note)
        n += 1
    return n

root = add(None, name="OpenWalkieTalkie Rev B 电路板组件（主板 + 编码器板）", unit="套",
           spec="每套含主板 PCBA 1 块、编码器板 PCBA 1 块", note="两块板分别制板、分别贴装。")
main = add(root, name="主板 PCBA（Rev B）", unit="块", spec="4 层，53.43 × 92.43 mm，板厚 1.0 mm；双面贴装",
           part="main-revb-gerber.zip / main-revb-cpl.csv",
           note="不贴装（Gerber 中有焊盘）：C52、C53、C58、L6、R44、测试点 TP1–TP10。")
add(main, name="主板 PCB 裸板", unit="块", part="main-revb-gerber.zip",
    spec="4 层，板厚 1.0 mm，沉金（ENIG），外层 1 oz / 内层 0.5 oz，最小孔 0.20 mm / 焊环 0.45 mm，最小线宽/线距 0.12 / 0.10 mm，过孔树脂塞孔 + 电镀盖帽",
    rule="叠层与板厚不可更改，见“PCB 工艺要求”", note="48 个过孔在贴片焊盘内或紧贴焊盘，必须树脂塞孔并盖帽。")
n_main = add_parts(main, design_main, jlc_main, SPECIAL, EXTRA)
main_first, main_last = len(rows) - n_main, len(rows) - 1
enc = add(root, name="编码器板 PCBA（encoder-wheel r6）", unit="块", spec="2 层，21.35 × 20.39 mm，板厚 1.0 mm；双面贴装",
          part="encoder-wheel-gerber.zip / encoder-wheel-cpl.csv", note="ENC1 贴于底面，J1 贴于顶面。板上丝印 “OMD ENC-W r6”。")
add(enc, name="编码器板 PCB 裸板", unit="块", part="encoder-wheel-gerber.zip",
    spec="2 层，板厚 1.0 mm，沉金（ENIG），最小孔 0.20 mm / 焊环 0.45 mm，过孔盖油", rule="板厚不可更改")
n_enc = add_parts(enc, design_enc, jlc_enc, SPECIAL_ENC, {})

qty_main = sum(rows[i]["qty"] for i in range(main_first, main_last + 1))
top_main = sum(d["top"] for d in jlc_main.values())
bot_main = sum(d["bottom"] for d in jlc_main.values())
if (n_main, qty_main, top_main, bot_main) != (51, 125, 60, 65):
    sys.exit(f"main board totals changed: {n_main} lines, {qty_main} parts, top {top_main}, bottom {bot_main}")

children = {}
for i, r in enumerate(rows):
    children.setdefault(r["parent"], []).append(i)
code = {}
def number(i, prefix):
    code[i] = prefix
    for n, c in enumerate(children.get(i, []), 1):
        number(c, f"{prefix}.{n}")
number(root, "1")
level = {i: code[i].count(".") for i in range(len(rows))}

# ---------------------------------------------------------------- workbook
FONT = "Arial"
thin = Side(style="thin", color="BFBFBF")
border = Border(left=thin, right=thin, top=thin, bottom=thin)
HEAD_FILL = PatternFill("solid", fgColor="1F3864")

def header(ws, names, widths):
    ws.append(names)
    for c, w in enumerate(widths, 1):
        cell = ws.cell(row=1, column=c)
        cell.font = Font(name=FONT, bold=True, color="FFFFFF", size=10)
        cell.fill = HEAD_FILL
        cell.alignment = Alignment(horizontal="center", vertical="center", wrap_text=True)
        cell.border = border
        ws.column_dimensions[get_column_letter(c)].width = w
    ws.row_dimensions[1].height = 30

def style(ws, first_row, last_row, ncol, wrap=(), center=(), bold_col1=False):
    for row in ws.iter_rows(min_row=first_row, max_row=last_row, max_col=ncol):
        for cell in row:
            cell.font = Font(name=FONT, size=10, bold=(bold_col1 and cell.column == 1))
            cell.border = border
            cell.alignment = Alignment(vertical="top", wrap_text=cell.column in wrap,
                                       horizontal="center" if cell.column in center else "left")

wb = Workbook()
ws = wb.active
ws.title = "多级BOM"
hdr = ["层级", "物料编号", "物料名称", "规格 / 描述", "位号", "单位", "单级用量", "每套用量", "封装", "贴装面",
       "实际贴装品牌", "实际贴装型号", "立创编号 / 文件", "设计品牌", "设计型号", "用料状态", "替代要求", "备注"]
header(ws, hdr, [6, 9, 30, 46, 30, 6, 8, 8, 22, 14, 20, 26, 24, 12, 24, 11, 44, 50])
STATUS_COLOR = {"库存改料": "C00000", "预定替代": "7F6000"}
LEVEL_FILL = {0: "D9E1F2", 1: "EDEDED"}
xrow = {}
for i, r in enumerate(rows):
    rr = i + 2
    xrow[i] = rr
    lv = level[i]
    ws.append([lv, code[i], r["name"], r["spec"], r["refs"], r["unit"], r["qty"], None, r["pkg"], r["side"], r["brand"],
               r["mpn"], r["part"], r["dbrand"], r["dmpn"], r["status"], r["rule"], r["note"]])
    ws.cell(row=rr, column=8).value = f"=G{rr}" if r["parent"] is None else f"=H{xrow[r['parent']]}*G{rr}"
    for c in range(1, len(hdr) + 1):
        cell = ws.cell(row=rr, column=c)
        cell.font = Font(name=FONT, size=10, bold=lv <= 1, color=STATUS_COLOR.get(r["status"], "000000") if c == 16 else "000000")
        cell.border = border
        cell.alignment = Alignment(vertical="top", wrap_text=c in (3, 4, 5, 9, 11, 12, 13, 15, 17, 18),
                                   horizontal="center" if c in (1, 2, 6, 7, 8, 16) else "left")
        if lv in LEVEL_FILL:
            cell.fill = PatternFill("solid", fgColor=LEVEL_FILL[lv])
        elif r["status"] == "库存改料":
            cell.fill = PatternFill("solid", fgColor="FFF2CC")
    ws.cell(row=rr, column=3).alignment = Alignment(vertical="top", wrap_text=True, indent=lv * 2)
    if lv >= 1:
        ws.row_dimensions[rr].outlineLevel = lv
ws.sheet_properties.outlinePr = Outline(summaryBelow=False, summaryRight=False)
ws.freeze_panes = "D2"
ws.auto_filter.ref = f"A1:{get_column_letter(len(hdr))}{len(rows) + 1}"
chk = len(rows) + 3
for n, (label, formula) in enumerate([
        ("校验：主板贴装元件合计（应为 125）", f"=SUM(G{xrow[main_first]}:G{xrow[main_last]})"),
        ("校验：主板 BOM 行数（应为 51）", f"=COUNTA(C{xrow[main_first]}:C{xrow[main_last]})")]):
    ws.cell(row=chk + n, column=3, value=label).font = Font(name=FONT, size=10, bold=True)
    ws.cell(row=chk + n, column=8, value=formula).font = Font(name=FONT, size=10, bold=True)
ws.cell(row=chk + 2, column=3, value="图例：黄色底 = 上批因库存改用的替代料（红字“库存改料”）；“预定替代” = 设计阶段已选定的同规格料。").font = Font(name=FONT, size=9, italic=True)

# ---- substitutions
sub = wb.create_sheet("替代料清单")
header(sub, ["板", "位号", "数量", "物料名称", "设计品牌", "设计型号", "实际贴装品牌", "实际贴装型号", "立创编号", "用料状态", "替代要求", "备注"],
       [10, 30, 6, 30, 12, 24, 20, 26, 12, 11, 44, 50])
for i, r in enumerate(rows):
    if r["status"] in ("库存改料", "预定替代"):
        board = "主板" if main_first <= i <= main_last else "编码器板"
        sub.append([board, r["refs"], r["qty"], r["name"], r["dbrand"], r["dmpn"], r["brand"], r["mpn"], r["part"], r["status"], r["rule"], r["note"]])
style(sub, 2, sub.max_row, 12, wrap=(2, 4, 6, 7, 8, 11, 12), center=(3, 10))
for row in sub.iter_rows(min_row=2, max_row=sub.max_row, max_col=12):
    if row[9].value == "库存改料":
        for cell in row:
            cell.fill = PatternFill("solid", fgColor="FFF2CC")
sub.freeze_panes = "A2"

# ---- PCB requirements
PCB = [
    ("项目", "主板（Rev B）", "编码器板（encoder-wheel r6）"),
    ("Gerber 文件", "main-revb-gerber.zip（13 个文件）", "encoder-wheel-gerber.zip（11 个文件）"),
    ("层数", "4 层", "2 层"),
    ("外形尺寸", "53.43 × 92.43 mm（异形，见 Edge_Cuts）", "21.35 × 20.39 mm（异形，见 Edge_Cuts）"),
    ("成品板厚", "1.0 mm（不可改为 1.6 mm，外壳按 1.0 mm 设计）", "1.0 mm"),
    ("叠层", "L1 1 oz / PP 3313 0.0994 mm / L2 0.5 oz / 芯板 0.7 mm / L3 0.5 oz / PP 3313 0.0994 mm / L4 1 oz（嘉立创 JLC04101H-3313）。L2、L3 为完整地平面", "标准 2 层，1 oz"),
    ("阻抗", "B 面（L4）一条 50 Ω 共面波导射频线：线宽 0.16 mm，两侧间隙 0.20 mm，参考 L3 地平面。按上述叠层设计；如贵司叠层不同，请告知并由我方确认后再生产", "无"),
    ("表面处理", "沉金（ENIG）", "沉金（ENIG）"),
    ("最小孔 / 焊环", "0.20 mm / 0.45 mm（共 477 个过孔）", "0.20 mm / 0.45 mm（5 个过孔）"),
    ("最小线宽 / 线距", "0.12 mm / 0.10 mm", "见 Gerber"),
    ("过孔工艺", "树脂塞孔 + 电镀盖帽（POFV）。48 个过孔在贴片焊盘内或紧贴焊盘（U5、U6、U7、U8 散热焊盘等），不可开窗或仅盖油", "过孔盖油"),
    ("阻焊 / 字符", "颜色见询价邮件；字符白色", "同主板"),
    ("生产标记", "板内不加生产编号 / 周期等标记；如必须加，请先确认位置", "同主板"),
    ("拼板 / 工艺边", "可由贵司拼板并加工艺边和光学定位点（板内无 Mark 点）。USB-C 座和侧按开关伸出板边，工艺边需避让", "可由贵司拼板并加工艺边"),
    ("贴装", f"双面贴装，共 {qty_main} 个元件 / {n_main} 行：顶面（F，电池侧）{top_main} 个，底面（B，显示屏侧）{bot_main} 个", "双面贴装，共 2 个元件：ENC1 在底面，J1 在顶面"),
    ("不贴装", "C52、C53、C58、L6（射频调谐位）、R44、测试点 TP1–TP10。这些位置不印锡膏", "无"),
    ("钢网", "顶面、底面锡膏层在 Gerber 包内（F_Paste / B_Paste）", "同左"),
    ("坐标文件", "main-revb-cpl.csv", "encoder-wheel-cpl.csv"),
]
ps = wb.create_sheet("PCB工艺要求")
header(ps, list(PCB[0]), [18, 70, 50])
for r in PCB[1:]:
    ps.append(list(r))
style(ps, 2, ps.max_row, 3, wrap=(2, 3), bold_col1=True)

# ---- notes
changed = [r for r in rows if r["status"] == "库存改料"]
NOTES = [
    ("文件", "OpenWalkieTalkie Rev B 电路板组件询价资料（多级 BOM）"),
    ("日期", args.date),
    ("内容", "主板（Rev B）与编码器板（encoder-wheel r6）两块板的 PCB 制板 + SMT 贴装。不含显示屏、电池、扬声器、外壳等整机物料。"),
    ("层级规则", "层级 0 一套电路板；层级 1 两块 PCBA；层级 2 裸板与各贴装元件。物料编号按 1 / 1.x / 1.x.y 级联。每套用量 = 上级每套用量 × 单级用量（H 列公式）。"),
    ("型号列", "“实际贴装品牌 / 型号 / 立创编号”为上一批在嘉立创实际贴装的物料，请按此列报价；“设计品牌 / 型号”为原设计型号，可作为备选。"),
    ("用料状态", "与设计一致：实际贴装即设计型号。预定替代：设计阶段已选定的同规格料。库存改料：上批因嘉立创库存不足改用的替代料（黄色底），共 "
                 f"{len(changed)} 行：" + "、".join(r["refs"].replace(", ", "/") for r in changed) + "。详见“替代料清单”。"),
    ("替代原则", "阻容件可按“替代要求”列用同规格品牌料替代并在报价中列明；IC、连接器、开关、电感、天线、晶振、麦克风为指定型号，缺货或需替代时请先与我方确认。"),
    ("客供料", "U5（SiFli SF32LB525UC6）上批由我方提供。请说明贵司可否代采；如不能，仍按客供料处理。"),
    ("坐标文件约定", "单位 mm，原点为 Gerber 原点，Y 取负值。顶面元件：旋转角 = KiCad 角度；底面元件：X 不镜像，旋转角 = 180 − KiCad 角度（嘉立创约定）。坐标为封装原点，个别连接器的原点不在焊盘中心，偏移量见 pin1-check 文件。"),
    ("极性核对", "main-revb-pin1-check.csv / encoder-wheel-pin1-check.csv 列出每个有方向元件的 1 脚位置（从顶面看）。所有 IC 与晶振在丝印上有 1 脚圆点（U6 为三角标记）。"),
    ("贴装注意", "MK1 麦克风贴于底面，进音孔朝外，回流后不可水洗；J5（USB-C）外壳定位脚为通孔；J5 与 SW1/SW2/SW5/SW6 伸出板边。"),
    ("测试", "不要求贵司做功能测试与烧录；如可提供 AOI / X-ray（U5、U6、U7、U8 为 QFN/DFN 带散热焊盘），请在报价中列明。"),
    ("报价请列明", "PCB 费用、钢网费、元件费（逐行，注明替代料）、贴装加工费、工程费、交期、最小起订量。"),
]
ns = wb.create_sheet("说明")
header(ns, ["项目", "内容"], [16, 120])
for k, v in NOTES:
    ns.append([k, v])
style(ns, 2, ns.max_row, 2, wrap=(2,), bold_col1=True)

wb.calculation.fullCalcOnLoad = True

# ---------------------------------------------------------------- package
OUT.mkdir(parents=True, exist_ok=True)
bom = OUT / "OpenWalkieTalkie-RevB-BOM-zh.xlsx"
wb.save(bom)
COPIES = [
    (REVB / "fab-jlcpcb/main-revb-jlcpcb-gerber.zip", "main-revb-gerber.zip"),
    (REVB / "fab-jlcpcb/main-cpl-jlcpcb.csv", "main-revb-cpl.csv"),
    (REVB / "fab-jlcpcb/main-orientation-check.csv", "main-revb-pin1-check.csv"),
    (ENC / "encoder-wheel-jlcpcb-gerber.zip", "encoder-wheel-gerber.zip"),
    (ENC / "encoder-wheel-cpl-jlcpcb.csv", "encoder-wheel-cpl.csv"),
    (ENC / "encoder-wheel-orientation-check.csv", "encoder-wheel-pin1-check.csv"),
]
for src, name in COPIES:
    shutil.copyfile(src, OUT / name)

readme = [f"OpenWalkieTalkie Rev B 电路板组件 询价资料（{args.date}）", "=" * 50, "",
          "文件清单", "--------",
          "OpenWalkieTalkie-RevB-BOM-zh.xlsx   多级 BOM（实际贴装型号 + 设计型号）、替代料清单、PCB 工艺要求、说明",
          "main-revb-gerber.zip                主板 Rev B Gerber + 钻孔（4 层，含锡膏层）",
          "main-revb-cpl.csv                   主板贴片坐标文件",
          "main-revb-pin1-check.csv            主板有方向元件 1 脚位置核对表",
          "encoder-wheel-gerber.zip            编码器板 Gerber + 钻孔（2 层，含锡膏层）",
          "encoder-wheel-cpl.csv               编码器板贴片坐标文件",
          "encoder-wheel-pin1-check.csv        编码器板有方向元件 1 脚位置核对表", "",
          "PCB 工艺要求", "------------"]
for item, a, b in PCB[1:]:
    readme += [f"【{item}】", f"  主板：{a}", f"  编码器板：{b}"]
readme += ["", "说明", "----"] + [f"【{k}】{v}" for k, v in NOTES[2:]]
(OUT / "README-zh.txt").write_text("﻿" + "\r\n".join(readme) + "\r\n", encoding="utf-8")

pkg = OUT.with_suffix(".zip")
with zipfile.ZipFile(pkg, "w", zipfile.ZIP_DEFLATED) as z:
    for f in sorted(OUT.iterdir()):
        z.write(f, f"{OUT.name}/{f.name}")
print(f"lines main={n_main} encoder={n_enc} parts main={qty_main} (top {top_main}, bottom {bot_main}) changed={len(changed)}")
for r in rows:
    if r["status"] != "与设计一致" and r["status"]:
        print(f"  {r['status']}  {r['refs'][:28]:28}  {r['dbrand']} {r['dmpn']}  ->  {r['brand']} {r['mpn']} ({r['part']})")
print(f"saved {OUT} and {pkg}")
