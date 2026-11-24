"""Create the Chinese magnet RFQ drawing from the current HW17-R0 geometry.
PDF dimensions are vector graphics; DXF geometry is exact, 1:1, in millimetres.
"""
from pathlib import Path
import math, json, hashlib, shutil, zipfile
import ezdxf
from ezdxf.enums import TextEntityAlignment
from reportlab.pdfgen import canvas
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.lib.colors import HexColor, Color, black, white
from reportlab.lib.units import mm
from pypdf import PdfReader

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
PDF_DIR=ROOT/'output/pdf'
PDF_DIR.mkdir(parents=True,exist_ok=True)
SOURCE=HERE.parent/'d17x9-fusion/source/design.json'
G=json.loads(SOURCE.read_text())['geometry']
RO=G['magnet_od_mm']/2;RI=G['magnet_id_mm']/2;H=G['magnet_height_mm']
ANGLE=360/G['magnet_poles']-G['magnet_joint_gap_deg']
HALF=ANGLE/2;THETA=math.radians(ANGLE)
A0=90-HALF;A1=90+HALF
CO=2*RO*math.sin(THETA/2);CI=2*RI*math.sin(THETA/2)
LO=RO*THETA;LI=RI*THETA
PDF=PDF_DIR/'HW17-MAG-RFQ-A_磁瓦询价图.pdf'
pdfmetrics.registerFont(TTFont('CN','/System/Library/Fonts/STHeiti Light.ttc',subfontIndex=0))
pdfmetrics.registerFont(TTFont('CNB','/System/Library/Fonts/STHeiti Medium.ttc',subfontIndex=0))
INK=HexColor('#182730');BLUE=HexColor('#174f68');MUTED=HexColor('#52636d')
LIGHT=HexColor('#edf3f6');RED=HexColor('#a63e37')
TEXT_BOXES=[]
c=canvas.Canvas(str(PDF),pagesize=(420*mm,297*mm),pageCompression=1)
c.setTitle('HW17-MAG-RFQ-A | N35SH 磁瓦尺寸与材料询价图')
c.setAuthor('OpenWalkieTalkie')
c.setSubject('RFQ only; proposed tolerances require supplier confirmation')

def text(x,y,s,size=10.5,color=INK,bold=False,align='left',angle=0):
    font='CNB' if bold else 'CN'
    c.saveState();c.setFillColor(color);c.setFont(font,size)
    if angle:
        c.translate(x*mm,y*mm);c.rotate(angle);x=y=0
    if align=='center':c.drawCentredString(x*mm,y*mm,s)
    elif align=='right':c.drawRightString(x*mm,y*mm,s)
    else:c.drawString(x*mm,y*mm,s)
    c.restoreState()
    if not angle:
        w=pdfmetrics.stringWidth(s,font,size)/mm
        left=x if align=='left' else x-w/2 if align=='center' else x-w
        TEXT_BOXES.append((c.getPageNumber(),left,y-size/mm*.25,left+w,y+size/mm))

def lines(x,y,s,width,size=10.5,leading=5.0,color=INK):
    """Wrap by measured glyph widths; caller owns the available vertical space."""
    out=[]
    for paragraph in s.split('\n'):
        line=''
        for ch in paragraph:
            if pdfmetrics.stringWidth(line+ch,'CN',size)>width*mm and line:
                out.append(line);line=ch
            else:line+=ch
        out.append(line)
    for line in out:text(x,y,line,size,color);y-=leading
    return y

def line(x1,y1,x2,y2,width=.22,color=INK,dash=None):
    c.saveState();c.setStrokeColor(color);c.setLineWidth(width*mm)
    if dash:c.setDash([d*mm for d in dash])
    c.line(x1*mm,y1*mm,x2*mm,y2*mm);c.restoreState()

def box(x,y,w,h,fill=None,width=.22,color=INK):
    c.saveState();c.setStrokeColor(color);c.setLineWidth(width*mm)
    if fill:c.setFillColor(fill)
    c.rect(x*mm,y*mm,w*mm,h*mm,fill=bool(fill),stroke=1);c.restoreState()

def path_poly(points,fill=None,width=.25,color=INK,close=True):
    p=c.beginPath();p.moveTo(points[0][0]*mm,points[0][1]*mm)
    for x,y in points[1:]:p.lineTo(x*mm,y*mm)
    if close:p.close()
    c.setLineWidth(width*mm);c.setStrokeColor(color)
    if fill:c.setFillColor(fill)
    c.drawPath(p,stroke=1,fill=bool(fill))

def arc(cx,cy,r,start,end,width=.22,color=INK):
    c.saveState();c.setStrokeColor(color);c.setLineWidth(width*mm)
    c.arc((cx-r)*mm,(cy-r)*mm,(cx+r)*mm,(cy+r)*mm,startAng=start,extent=end-start)
    c.restoreState()

def polar(cx,cy,r,a):return (cx+r*math.cos(math.radians(a)),cy+r*math.sin(math.radians(a)))

def arrow(a,b,size=1.5,color=INK,width=.18,shaft=True):
    if shaft:line(*a,*b,width,color)
    dx,dy=b[0]-a[0],b[1]-a[1];l=math.hypot(dx,dy);ux,uy=dx/l,dy/l
    tail=(b[0]-size*ux,b[1]-size*uy)
    path_poly([b,(tail[0]-.35*size*uy,tail[1]+.35*size*ux),(tail[0]+.35*size*uy,tail[1]-.35*size*ux)],color,0,color)

def dimh(x1,x2,y,fromy,label,size=10):
    for x in [x1,x2]:line(x,fromy,x,y+1,.13)
    line(x1,y,x2,y,.15)
    arrow((x1+2.5,y),(x1,y),1.6,shaft=False)
    arrow((x2-2.5,y),(x2,y),1.6,shaft=False)
    text((x1+x2)/2,y+2.4,label,size,align='center')

def dimv(x,y1,y2,fromx,label,size=10):
    for y in [y1,y2]:line(fromx,y,x+1,y,.13)
    line(x,y1,x,y2,.15)
    arrow((x,y1+3),(x,y1),1.6,shaft=False)
    arrow((x,y2-3),(x,y2),1.6,shaft=False)
    text(x+3,(y1+y2)/2,label,size,align='center',angle=90)

def segment(cx,cy,scale,fill=LIGHT,start=A0,end=A1):
    # PDF circular arcs use cubic Beziers via ReportLab's exact arc command.
    p=c.beginPath()
    x,y=polar(cx,cy,RO*scale,start);p.moveTo(x*mm,y*mm)
    p.arcTo((cx-RO*scale)*mm,(cy-RO*scale)*mm,(cx+RO*scale)*mm,(cy+RO*scale)*mm,startAng=start,extent=end-start)
    x,y=polar(cx,cy,RI*scale,end);p.lineTo(x*mm,y*mm)
    p.arcTo((cx-RI*scale)*mm,(cy-RI*scale)*mm,(cx+RI*scale)*mm,(cy+RI*scale)*mm,startAng=end,extent=start-end)
    p.close();c.setFillColor(fill);c.setStrokeColor(INK);c.setLineWidth(.28*mm)
    c.drawPath(p,stroke=1,fill=1)

def hatch_rect(x,y,w,h):
    c.saveState();p=c.beginPath();p.rect(x*mm,y*mm,w*mm,h*mm);c.clipPath(p,stroke=0)
    c.setFillColor(LIGHT);c.rect(x*mm,y*mm,w*mm,h*mm,fill=1,stroke=0)
    for d in range(-int(h)-5,int(w)+5,3):line(x+d,y,x+d+h,y+h,.10,MUTED)
    c.restoreState();box(x,y,w,h)

def header(page,title):
    box(10,10,400,277,width=.35)
    text(16,277,title,18,bold=True)
    text(16,268,'OpenWalkieTalkie  /  HW17 自研力反馈滚轮  /  单件磁瓦，非整体磁环',10.5,MUTED)
    box(323,266,81,15,HexColor('#fff2ee'),.25,RED)
    text(363.5,275,'RFQ-A  询价用',12,RED,True,'center')
    text(363.5,269,'公差与工艺待供应商回签',9,RED,align='center')
    line(10,261,410,261,.35)

def titleblock(page):
    box(250,10,160,49)
    text(254,51,'HW17-MAG-RFQ-A',13,bold=True)
    text(254,44,'磁瓦  |  -N / -S 两种极性共用几何',10.5)
    line(250,40,410,40,.18)
    text(254,34,'材料 N35SH   |   表面与磁性能见第 2 页',10)
    text(254,27,'单位 mm   |   尺寸视图 10:1   |   图幅 A3',10)
    line(250,23,410,23,.18)
    text(254,16,'2026-10-29    编制：OpenWalkieTalkie',9.5)
    text(405,16,f'{page} / 2',10,align='right')

header(1,'磁瓦二维尺寸图')
text(20,250,'端视图 A（沿轴向 Z 看）',12,BLUE,True)
text(20,244,'内外弧同心，两端面均沿半径方向',9.5,MUTED)
cx,cy,sc=87,129,10
segment(cx,cy,sc)
line(cx,cy-7,cx,cy+RO*sc+9,.12,MUTED,[4,1,1,1])
line(cx-6,cy,cx+6,cy,.12,MUTED,[4,1,1,1])
for a in [A0,A1]:line(cx,cy,*polar(cx,cy,RO*sc+6,a),.12,MUTED,[3,1])
text(cx-5,cy-8,'O',10)
outery=cy+RO*sc*math.cos(THETA/2)
innery=cy+RI*sc*math.cos(THETA/2)
dimh(cx-CO*sc/2,cx+CO*sc/2,225,outery,f'({CO:.3f}) 外弦长 REF')
dimh(cx-CI*sc/2,cx+CI*sc/2,184,innery,f'({CI:.3f}) 内弦长 REF',9.5)
tip=polar(cx,cy,RO*sc,81)
arrow((124,210),tip);line(124,210,168,210,.15)
text(124,213,'R7.650 ±0.020',11,bold=True)
tip=polar(cx,cy,RI*sc,99)
arrow((51,208),tip);line(20,208,51,208,.15)
text(20,211,'R6.750 ±0.020',11,bold=True)
rr=34
arc(cx,cy,rr,A0,A1,.16)
arrow(polar(cx,cy,rr,A0+4),polar(cx,cy,rr,A0),1.3,shaft=False)
arrow(polar(cx,cy,rr,A1-4),polar(cx,cy,rr,A1),1.3,shaft=False)
c.setFillColor(white);c.rect((cx-24)*mm,149*mm,48*mm,6*mm,fill=1,stroke=0)
text(cx,151,f'{ANGLE:.4f}° ±0.10°',11,bold=True,align='center')
text(20,112,'径向厚度 t = (0.900) REF',10.5)
text(20,105,'半径尺寸定义整圆轮廓，不是磁瓦宽度。',10,MUTED)

text(208,250,'正视图 B（从外弧看）',12,BLUE,True)
fcx,fy=250,165
box(fcx-CO*5,fy,CO*10,H*10,HexColor('#f6f8fa'),.28)
for x in [fcx-CI*5,fcx+CI*5]:line(x,fy,x,fy+H*10,.12,MUTED,[2,1])
line(fcx,fy-4,fcx,fy+H*10+5,.12,MUTED,[4,1,1,1])
dimh(fcx-CO*5,fcx+CO*5,225,fy+H*10,f'({CO:.3f}) REF')
dimv(284,fy,fy+H*10,fcx+CO*5,'4.500 ±0.050',11)
arrow((308,169),(308,210),2,BLUE,.25)
text(308,216,'Z / 轴向',9.5,BLUE,align='center')
text(fcx,155,'虚线为内弧端点投影',9,MUTED,align='center')

text(335,250,'中心径向截面',12,BLUE,True)
text(335,244,'通过端视图对称中心线',9.5,MUTED)
hatch_rect(352,fy,(RO-RI)*sc,H*sc)
dimh(352,361,225,210,'(0.900) REF',10)
text(356.5,155,'高度同正视图',9.5,MUTED,align='center')

text(250,137,'参考尺寸（括号 / REF，非独立验收尺寸）',10.5,BLUE,True)
rows=[('外径',f'Φ{RO*2:.3f}','内径',f'Φ{RI*2:.3f}'),('外弧长',f'{LO:.3f}','内弧长',f'{LI:.3f}'),('外弦长',f'{CO:.3f}','内弦长',f'{CI:.3f}'),('径向厚度','0.900','单片半角',f'{HALF:.4f}°')]
for i,row in enumerate(rows):
    yy=125-i*11
    box(250,yy-3,160,11,white if i%2 else LIGHT,.12)
    for x,s in zip([254,284,331,366],row):text(x,yy,s,10)

text(20,90,'尺寸与加工说明',12,BLUE,True)
notes=[
'1. 所有标注尺寸均为涂层后成品尺寸；括号尺寸仅作参考。',
'2. R、轴向高度及圆心角公差为询价暂定值，供应商须回签能力。',
'3. 不允许裂纹、明显崩边及涂层露底；边缘倒钝最大 0.05，待确认。',
'4. DXF 为未倒钝的名义轮廓，单位 mm、比例 1:1；尺寸以标注为准。',
'5. -N 与 -S 尺寸相同，仅极性相反；极性定义与数量见第 2 页。',
'6. 本图为供应商询价输入，样品下单前须确认图纸、公差及充磁工艺。']
y=81
for s in notes:y=lines(20,y,s,220,10.5,5.2)-2.3
text(20,19,'几何来源：HW17-R0 Fusion 原生模型（2026-10-29）；严禁按图量取。',9,MUTED)
titleblock(1)
c.showPage()

header(2,'磁瓦牌号、充磁与询价要求')
text(20,250,'01  材料要求（20°C）',12,BLUE,True)
material=[
('牌号 / 工艺','N35SH / 烧结、各向异性钕铁硼'),
('剩磁 Br','≥1.17 T；典型约 1.21 T'),
('磁感矫顽力 HcB','≥876 kA/m（11.0 kOe）'),
('内禀矫顽力 Hcj','≥1592 kA/m（20.0 kOe）'),
('最大磁能积 (BH)max','≥263 kJ/m³（33 MGOe），35 等级'),
('温度等级说明','SH 常见参考上限 150°C，非本电机额定值'),
('替代要求','普通 N35、N35M、N35H 不可默认替代')]
for i,(k,v) in enumerate(material):
    yy=239-i*10
    box(20,yy-3,190,10,white if i%2 else LIGHT,.12)
    line(78,yy-3,78,yy+7,.12)
    text(23,yy,k,10);text(81,yy,v,9.7)
text(20,166,'35 表示磁能积等级；SH 表示高矫顽力等级。以上数值为询价要求。',9,MUTED)

text(20,154,'02  表面、交付与报价反馈',12,BLUE,True)
blocks=[
'涂层：优先黑色环氧，总膜厚目标 10-20 μm/单面（含底涂），供方确认。报价须注明完整涂层体系、固化及使用温度、胶接适配性。',
'交付：饱和充磁；极性 100% 检验。-N 与 -S 分袋/分格包装，标注料号、极性、数量和批次；不得靠磨削或刻槽破坏涂层来标极。',
'资料：提供牌号/批次证明及首件尺寸报告；提供 20/80/120/150°C 的 B-H、J-H 曲线（无该温度时注明）。磁通/磁矩一致性检验方法由供方提出。',
'温度：实际抗退磁能力取决于尺寸、磁路和反向磁场；150°C 不能直接作为本电机或环氧涂层的连续使用温度。',
'报价阶梯：10 / 100 / 1000 套，即 140 / 1400 / 14000 片；仅作价格比较，不是采购承诺。分别列单价、样品费、模具/充磁工装费、MOQ、交期及税运。']
y=144
for s in blocks:y=lines(20,y,s,190,10,4.8)-3
assert y>=37,y

text(225,250,'03  极性定义与真径向磁化示意',12,BLUE,True)
for x,inward,pn in [(265,True,'HW17-MAG-RFQ-A-N'),(360,False,'HW17-MAG-RFQ-A-S')]:
    oy=151;scale=10
    segment(x,oy,scale,HexColor('#fff0eb') if inward else HexColor('#edf5fb'))
    for ang in [82,90,98]:
        p0=polar(x,oy,7.43*scale,ang);p1=polar(x,oy,6.97*scale,ang)
        arrow(p0 if inward else p1,p1 if inward else p0,1.45,RED if inward else BLUE,.35)
    for ang in [A0,A1]:line(*polar(x,oy,6.65*scale-6,ang),*polar(x,oy,6.65*scale,ang),.12,MUTED,[2,1])
    text(x,238,'外弧 S' if inward else '外弧 N',10.5,align='center',bold=True)
    text(x,211,'内弧 N' if inward else '内弧 S',10.5,align='center',bold=True)
    text(x,244,pn,9,align='center')
    text(x,198,'每套 7 片',10.5,align='center')
text(225,184,'箭头为磁体内部磁化方向 M（由 S 指向 N）。',10)
text(225,178,'内外弧为两极；不得沿 4.5 mm 轴向或切向充磁。',10)
text(225,172,'红 / 蓝仅用于示意极性，实际涂层优先黑色环氧。',9.5,MUTED)

# Four-times nominal assembly layout, not a separately manufactured ring.
cx,cy=318,111
for i in range(14):
    mid=i*360/14
    segment(cx,cy,4,HexColor('#f5c6bd') if i%2==0 else HexColor('#bad9ed'),mid-ANGLE/2,mid+ANGLE/2)
    x,y=polar(cx,cy,(RO+RI)/2*4,mid)
    text(x,y-1,'N' if i%2==0 else 'S',8.3,align='center',bold=True)
text(cx,114,'14 片 / 套',13,bold=True,align='center')
text(cx,106,'标字指内弧极性',9,align='center')
text(225,72,'每台：内 N 7 片 + 内 S 7 片，沿圆周交替安装。',10.5,bold=True)
text(225,65,f'单片角 {ANGLE:.4f}°；节距 {360/14:.4f}°；拼缝 0.6°。',10)
text(225,59,'上图仅示意拼装；无需把 14 片粘成整体交付。',9.5,MUTED)
box(222,24,182,29,LIGHT,.2,BLUE)
lines(226,46,'主报价：真径向取向 + 真径向充磁，磁化方向随圆弧半径变化。若只能提供沿单片中心线平行取向/充磁，请作为备选单列报价并明确标注，不得默认替代。',174,10.5,5.5)

text(20,30,'参考资料（核查日期 2026-10-29；点击可打开）：',8.5,MUTED)
links=[('Arnold N35SH 数据表','https://www.arnoldmagnetics.com/wp-content/uploads/2017/11/N35SH-151021.pdf'),('Arnold 牌号与温度','https://www.arnoldmagnetics.com/products/neodymium-iron-boron-magnets/'),('K&J 充磁方向','https://www.kjmagnetics.com/magdir.asp'),('AOMAG 环氧涂层','https://www.aomagnet.com/neodymium_magnet_coating.html')]
for x,y,label,url in [(20,24,*links[0]),(101,24,*links[1]),(20,18,*links[2]),(101,18,*links[3])]:
    text(x,y,label,8.5,BLUE)
    w=pdfmetrics.stringWidth(label,'CN',8.5)
    c.linkURL(url,(x*mm,(y-1)*mm,x*mm+w,(y+3)*mm),relative=0)
text(405,16,'2 / 2',10,align='right')
c.save()

# Exact, closed arc profile. Start at outer right, CCW outer arc, inner arc CW.
bulge=math.tan(THETA/4)
verts=[(*polar(0,0,RO,A0),bulge),(*polar(0,0,RO,A1),0),(*polar(0,0,RI,A1),-bulge),(*polar(0,0,RI,A0),0)]
def doc_new():
    doc=ezdxf.new('R2010');doc.units=ezdxf.units.MM;doc.header['$MEASUREMENT']=1
    for name,col in [('PART_OUTLINE',7),('VIEW_FRONT',7),('VIEW_SECTION',7),('DIMENSIONS',3),('CENTER',8),('NOTES',7)]:doc.layers.new(name,dxfattribs={'color':col})
    doc.styles.new('RFQ',dxfattribs={'font':'Arial.ttf'})
    return doc
def add_profile(doc):return doc.modelspace().add_lwpolyline(verts,format='xyb',close=True,dxfattribs={'layer':'PART_OUTLINE'})
profile_doc=doc_new();add_profile(profile_doc)
PROFILE=HERE/'HW17-MAG-RFQ-A_profile_1to1.dxf';profile_doc.saveas(PROFILE)
doc=doc_new();m=doc.modelspace();add_profile(doc)
def dl(a,b,layer='DIMENSIONS'):m.add_line(a,b,dxfattribs={'layer':layer})
def dt(x,y,s,h=.35,layer='NOTES',center=False):
    e=m.add_text(s,dxfattribs={'height':h,'style':'RFQ','layer':layer})
    e.set_placement((x,y),align=TextEntityAlignment.MIDDLE_CENTER if center else TextEntityAlignment.LEFT)
def dh(x1,x2,y,fy,s):
    for x in [x1,x2]:dl((x,fy),(x,y+.1));dl((x-.08,y-.08),(x+.08,y+.08))
    dl((x1,y),(x2,y));dt((x1+x2)/2,y+.3,s,.32,'DIMENSIONS',True)
dt(-6,12,'HW17-MAG-RFQ-A | ARC MAGNET | RFQ ONLY',.5)
dt(-6,11.2,'UNITS mm | GEOMETRY 1:1 | FINAL COATED DIMENSIONS',.32)
for a in [A0,A1]:dl((0,0),polar(0,0,RO+.5,a),'CENTER')
dl((0,-.5),(0,8.5),'CENTER');dl((-.5,0),(.5,0),'CENTER')
dt(-.4,-.6,'O',.3)
dh(-CO/2,CO/2,9,RO*math.cos(THETA/2),f'({CO:.3f}) REF')
dl(polar(0,0,RO,81),(3.5,8.3));dl((3.5,8.3),(7,8.3));dt(3.5,8.5,'R7.650 +/-0.020',.35,'DIMENSIONS')
dl(polar(0,0,RI,99),(-3.4,7.5));dl((-3.4,7.5),(-6,7.5));dt(-6,7.8,'R6.750 +/-0.020',.35,'DIMENSIONS')
dt(0,3.0,f'{ANGLE:.4f} deg +/-0.10 deg',.35,'DIMENSIONS',True)
dt(0,-1.5,'END VIEW / AXIAL Z',.35,center=True)
fx,fy=13,5.0
m.add_lwpolyline([(fx-CO/2,fy),(fx+CO/2,fy),(fx+CO/2,fy+H),(fx-CO/2,fy+H)],close=True,dxfattribs={'layer':'VIEW_FRONT'})
dh(fx-CO/2,fx+CO/2,10,fy+H,f'({CO:.3f}) REF')
dx=16
for y in [fy,fy+H]:dl((fx+CO/2,y),(dx+.1,y));dl((dx-.08,y-.08),(dx+.08,y+.08))
dl((dx,fy),(dx,fy+H));dt(dx+.3,fy+H/2,'4.500 +/-0.050',.35,'DIMENSIONS')
dt(fx,4.2,'FRONT / FROM OUTER ARC',.35,center=True)
sx=25
m.add_lwpolyline([(sx,fy),(sx+.9,fy),(sx+.9,fy+H),(sx,fy+H)],close=True,dxfattribs={'layer':'VIEW_SECTION'})
dh(sx,sx+.9,10,fy+H,'(0.900) REF');dt(sx+.45,4.2,'RADIAL CENTER SECTION',.32,center=True)
notes=[
'MATERIAL: SINTERED ANISOTROPIC NdFeB N35SH. SEE PDF SHEET 2.',
'PRIMARY: TRUE RADIAL ORIENTATION AND MAGNETIZATION, POLES ON INNER / OUTER ARCS.',
'-N: INNER N / OUTER S, 7 PCS PER SET. -S: INNER S / OUTER N, 7 PCS PER SET.',
'EPOXY: TARGET TOTAL 10-20 um PER SURFACE INCLUDING UNDERCOAT; SUPPLIER TO CONFIRM.',
'PROPOSED TOLERANCES FOR RFQ ONLY. FINAL DRAWING / PROCESS APPROVAL BEFORE ORDER.',
'DXF NOMINAL PROFILE DOES NOT MODEL EDGE BREAK. MAX EDGE BREAK 0.05, TO BE AGREED.',
'REFERENCE: OD 15.300 / ID 13.500 / OUTER ARC 3.353 / INNER ARC 2.959 mm.',
'PARALLEL MAGNETIZATION MAY BE QUOTED SEPARATELY; DO NOT SUBSTITUTE WITHOUT APPROVAL.',
'CONTROLLED NOTES: HW17-MAG-RFQ-A PDF, 2 SHEETS, 2026-10-29.'
]
for i,s in enumerate(notes):dt(-6,-3-i*.65,s,.32)
DXF=HERE/'HW17-MAG-RFQ-A_dimensioned_1to1.dxf';doc.saveas(DXF)

rfq='''供应商您好，请按附件 HW17-MAG-RFQ-A 报价微型电机磁瓦。

1. 烧结、各向异性钕铁硼 N35SH；主方案为真径向取向、径向充磁。
2. 成品含涂层尺寸：外 R7.650±0.020、内 R6.750±0.020、轴向高 4.500±0.050 mm；单片圆心角 25.1143°±0.10°。这些为询价暂定公差，请回复可实现公差和成本影响。
3. 两种极性同尺寸：-N 内弧 N/外弧 S，每套7片；-S 内弧 S/外弧 N，每套7片。每套共14片，供货不必粘成磁环。
4. 20°C 材料指标：Br≥1.17 T，HcB≥876 kA/m，Hcj≥1592 kA/m，(BH)max≥263 kJ/m³。提供牌号/批次证明及温度退磁曲线。普通N35/N35M/N35H不可直接替代。
5. 优先黑色环氧；总膜厚目标10-20 μm/单面（含底涂），请确认工艺能力，并说明完整涂层体系、耐温与胶接适配性。所有尺寸以涂层后为准。
6. 饱和充磁交付，100%检查极性；N/S分开包装并标注极性、料号、数量和批次。
7. 请分别报10/100/1000套（140/1400/14000片），并列样品费、模具/充磁工装费、MOQ、交期、含税/运费条件。以上仅询价阶梯，不构成采购订单。
8. 请明确“真径向取向/充磁”能否实现；如果只能沿单片中心线平行取向/充磁，请作为备选单列，不得默认替换。

附件PDF包含尺寸图、极性图和材料要求。DXF为mm、1:1名义轮廓；具体尺寸、公差与备注以PDF为准。下单前双方再确认最终图纸。
'''
(HERE/'供应商询价文字.txt').write_text(rfq,encoding='utf-8')

# Structural QA, quantities and dimensions checked independently after write.
rd=ezdxf.readfile(PROFILE);poly=list(rd.modelspace().query('LWPOLYLINE'))
assert rd.units==4 and len(poly)==1 and poly[0].closed
pts=list(poly[0].get_points('xyb'))
assert len(pts)==4 and abs(pts[0][2]-bulge)<1e-12 and abs(pts[2][2]+bulge)<1e-12
assert abs(math.hypot(*pts[0][:2])-RO)<1e-12 and abs(math.hypot(*pts[2][:2])-RI)<1e-12
assert not rd.audit().has_errors and not ezdxf.readfile(DXF).audit().has_errors
reader=PdfReader(PDF);assert len(reader.pages)==2
texts=[p.extract_text() for p in reader.pages]
for t in ['R7.650','R6.750','4.500','25.1143','N35SH']:
    assert t in ''.join(texts),t
assert all(8<=b[1] and b[3]<=412 and 8<=b[2] and b[4]<=289 for b in TEXT_BOXES)
for page in reader.pages:
    assert abs(float(page.mediabox.width)-420*mm)<.01
report={'drawing':'HW17-MAG-RFQ-A','geometry_source':str(SOURCE.relative_to(ROOT)),
 'source_sha256':hashlib.sha256(SOURCE.read_bytes()).hexdigest(),
 'geometry_mm':{'outer_radius':RO,'inner_radius':RI,'height':H,'thickness':RO-RI,'angle_deg':ANGLE,'outer_arc':LO,'inner_arc':LI,'outer_chord':CO,'inner_chord':CI},
 'proposed_rfq_tolerances':{'outer_radius_mm':.02,'inner_radius_mm':.02,'height_mm':.05,'angle_deg':.10},
 'grade':'N35SH','coating_target_um_per_surface':[10,20],'polarity_counts_per_set':{'inner_N':7,'inner_S':7},
 'pdf_pages':2,'pdf_page_size_mm':[420,297],'dxf_units':'mm','closed_exact_arc_profile':True,'dxf_audit_errors':0,
 'profile_area_mm2':.5*(RO**2-RI**2)*THETA,'part_volume_mm3':.5*(RO**2-RI**2)*THETA*H,
 'pdf_text_checks':'passed','pdf_margin_checks':'passed','visual_review':'pending'}
(HERE/'verification.json').write_text(json.dumps(report,indent=2,ensure_ascii=False),encoding='utf-8')
shutil.copy2(PDF,HERE/PDF.name)
print(json.dumps({'pdf':str(PDF),'dxf':str(DXF),'profile':str(PROFILE),'page_count':2,'part_volume_mm3':report['part_volume_mm3']},ensure_ascii=False,indent=2))
