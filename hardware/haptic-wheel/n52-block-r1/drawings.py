"""Dimensioned A3 supplier drawings, requirements, source-derived CAD sections.
PDFs and 1:1 DXFs are for RFQ, not an assertion of validated motor performance.
"""
from pathlib import Path
import os, math, json, shutil
os.environ.setdefault('MPLCONFIGDIR','/private/tmp/hw18-mpl')
import numpy as np
from PIL import Image
from reportlab.pdfgen import canvas
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.lib.colors import HexColor, white
from reportlab.lib.units import mm
from pypdf import PdfReader,PdfWriter
from OCP.BRepAlgoAPI import BRepAlgoAPI_Section
from OCP.BRepAdaptor import BRepAdaptor_Curve
from OCP.TopAbs import TopAbs_EDGE
from OCP.GeomAbs import GeomAbs_Line
from OCP.gp import gp_Pln,gp_Pnt,gp_Dir
from OCP.TopExp import TopExp_Explorer
from OCP.TopoDS import TopoDS
import build as cad

HERE=cad.HERE;ROOT=cad.ROOT;REV=cad.REV
OUT=HERE/'PDF';OUT.mkdir(exist_ok=True)
IMG=HERE/'images';IMG.mkdir(exist_ok=True)
PDFDIR=ROOT/'output/pdf';PDFDIR.mkdir(parents=True,exist_ok=True)
PDF=PDFDIR/'HW18-B1_N52方块磁铁电机_询价图纸及加工要求.pdf'
pdfmetrics.registerFont(TTFont('CN','/System/Library/Fonts/STHeiti Light.ttc',subfontIndex=0))
pdfmetrics.registerFont(TTFont('CB','/System/Library/Fonts/STHeiti Medium.ttc',subfontIndex=0))
INK=HexColor('#172d3d');MUTED=HexColor('#586f7c');BLUE=HexColor('#185d82');LIGHT=HexColor('#ecf2f6');RED=HexColor('#a14032')
c=canvas.Canvas(str(PDF),pagesize=(420*mm,297*mm),pageCompression=1)
c.setTitle('HW18-B1 | N52 标准方块磁铁台架电机 | 询价图纸及加工要求')
c.setAuthor('OpenMicroDial / Conol');c.setSubject('Bench prototype RFQ; explicit tolerance and inspection requirements')
TEXT=[];PAGES=[];PART_PAGE={};PAGE=0

def text(x,y,s,size=10,color=INK,bold=False,align='left'):
 s=str(s);font='CB' if bold else 'CN';c.setFont(font,size);c.setFillColor(color)
 if align=='center':c.drawCentredString(x*mm,y*mm,s)
 elif align=='right':c.drawRightString(x*mm,y*mm,s)
 else:c.drawString(x*mm,y*mm,s)
 w=pdfmetrics.stringWidth(s,font,size)/mm;left=x if align=='left' else x-w/2 if align=='center' else x-w
 TEXT.append({'page':PAGE,'text':s,'bbox':[left,y-size/mm*.24,left+w,y+size/mm*.85]})
def line(x1,y1,x2,y2,width=.2,color=INK,dash=None):
 c.saveState();c.setStrokeColor(color);c.setLineWidth(width*mm)
 if dash:c.setDash([d*mm for d in dash])
 c.line(x1*mm,y1*mm,x2*mm,y2*mm);c.restoreState()
def rect(x,y,w,h,fill=None,width=.2):
 c.setStrokeColor(INK);c.setLineWidth(width*mm)
 if fill:c.setFillColor(fill)
 c.rect(x*mm,y*mm,w*mm,h*mm,stroke=1,fill=bool(fill))
def poly(points,close=False,color=INK,width=.2,fill=None):
 if not points:return
 p=c.beginPath();p.moveTo(points[0][0]*mm,points[0][1]*mm)
 for x,y in points[1:]:p.lineTo(x*mm,y*mm)
 if close:p.close()
 c.setStrokeColor(color);c.setLineWidth(width*mm)
 if fill:c.setFillColor(fill)
 c.drawPath(p,stroke=1,fill=bool(fill))
def arrow(a,b,shaft=True):
 if shaft:line(*a,*b,.15)
 dx,dy=b[0]-a[0],b[1]-a[1];l=math.hypot(dx,dy);ux,uy=dx/l,dy/l;s=1.4
 poly([b,(b[0]-s*ux+.4*uy,b[1]-s*uy-.4*ux),(b[0]-s*ux-.4*uy,b[1]-s*uy+.4*ux)],True,fill=INK)
def dimh(x1,x2,y,fromy,label):
 for x in (x1,x2):line(x,fromy,x,y+1,.1,MUTED)
 line(x1,y,x2,y,.13);arrow((x1+3,y),(x1,y),False);arrow((x2-3,y),(x2,y),False)
 text((x1+x2)/2,y+2,label,9,align='center')
def dimv(y1,y2,x,fromx,label):
 for y in (y1,y2):line(fromx,y,x+1,y,.1,MUTED)
 line(x,y1,x,y2,.13);arrow((x,y1+3),(x,y1),False);arrow((x,y2-3),(x,y2),False)
 text(x+2,(y1+y2)/2,label,9)
def wrapped(x,y,s,w,size=10,leading=5):
 for para in s.split('\n'):
  buf=''
  for char in para:
   if pdfmetrics.stringWidth(buf+char,'CN',size)>w*mm and buf:
    text(x,y,buf,size);y-=leading;buf=''
   buf+=char
  text(x,y,buf,size);y-=leading
 return y
def notes(x,y,items,w=101,size=10):
 for i,item in enumerate(items,1):y=wrapped(x,y,f'{i}. {item}',w,size,5)-2
 return y
def table(x,y,widths,rows,rowh=6,size=9,header=True):
 top=y
 for i,row in enumerate(rows):
  xx=x
  for w,s in zip(widths,row):
   rect(xx,y-rowh,w,rowh,LIGHT if i==0 and header else None,.12)
   assert pdfmetrics.stringWidth(str(s),'CN',size)<(w-3)*mm,(PAGE,s,w)
   text(xx+1.5,y-rowh+1.7,s,size,bold=i==0 and header);xx+=w
  y-=rowh
 return y
def page(title,number,material='总成 / 技术要求',scale='按各视图标注',qty='-',pid=None):
 global PAGE
 PAGE+=1;PAGES.append({'page':PAGE,'drawing':number,'title':title})
 if pid:PART_PAGE.setdefault(pid,[]).append(PAGE-1)
 rect(10,10,400,277,width=.35)
 text(16,277,title,18,bold=True)
 text(16,269,'OpenMicroDial  /  N52 标准方块磁铁台架电机  /  HW18-B1  /  2026-10-30',10,MUTED)
 rect(324,267,80,15,HexColor('#fff1e9'))
 text(364,276,'样机询价版 · RFQ',12,RED,True,'center')
 text(364,270,'偏离图纸要求须逐项列出',9,RED,align='center')
 line(10,261,410,261,.3)
 rect(10,10,400,33)
 line(10,26,410,26,.15);line(190,10,190,43,.15);line(300,10,300,43,.15)
 text(15,34,number,12,bold=True)
 text(15,18,'单位 mm；成品尺寸含表面处理；不按打印图量尺寸。',9)
 wrapped(195,35,'材料：'+material,101,9,4)
 text(195,18,'数量：'+str(qty)+' / 台或套',9)
 text(305,35,'比例：'+scale+'  |  A3',9)
 text(305,18,f'版本 B1  /  询价文件第 {PAGE} 页',9)
def end():c.showPage()

def section_curves(shape,plane='xy',level=0):
 pl=gp_Pln(gp_Pnt(0,0,level),gp_Dir(0,0,1)) if plane=='xy' else gp_Pln(gp_Pnt(0,0,0),gp_Dir(0,1,0))
 sec=BRepAlgoAPI_Section(shape,pl,False);sec.Build();assert sec.IsDone()
 e=TopExp_Explorer(sec.Shape(),TopAbs_EDGE);out=[]
 while e.More():
  ad=BRepAdaptor_Curve(TopoDS.Edge(e.Current()));n=2 if ad.GetType()==GeomAbs_Line else 160
  pts=[]
  for t in np.linspace(ad.FirstParameter(),ad.LastParameter(),n):
   p=ad.Value(float(t));pts.append((p.X(),p.Y()) if plane=='xy' else (p.X(),p.Z()))
  out.append(pts);e.Next()
 return out
def view(shape,plane,level,cx,cy,scale,offset=0,color=INK):
 for pts in section_curves(shape,plane,level):poly([(cx+x*scale,cy+(y-offset)*scale) for x,y in pts],color=color,width=.22)

SPECS={
'P01':dict(od='Φ18.00±0.03',height='7.60±0.03',scale=5,rows=[
 ('外径 D1','18.00±0.03'),('内孔 D2','17.44 +0.03/0'),('轴向高度 H','7.60±0.03'),('内外孔口倒角','C0.10±0.05'),('外圆相对内孔跳动','≤0.05'),('端面平行度','≤0.03')],notes=[
 '基准A为内孔轴线，B为下端面。未注线性±0.05、角度±0.5°。',
 '内孔Ra≤1.6，外圆Ra≤3.2；不喷漆、不电镀。去毛刺，不得有尖边。',
 '与钢轭胶接滑配，名义单边间隙0.02；禁止强行压配使薄壁变形。',
 'POM表面须使用适配POM的底涂/胶接工艺；胶种与表面处理随报价说明。',
 '首轮用平滑外圈；如需纹理另报价，不在本模型内。']),
'P02':dict(od='Φ17.40 0/-0.01',height='6.80±0.02',scale=5,rows=[
 ('外径 D1','17.40 0/-0.01'),('十四平面到轴线距离','8.000±0.015，每面分别检验'),('对边尺寸（辅助检验）','16.000±0.030'),('十四面等分角','25.714286°；各面位置角±0.05°'),('总高 H','6.80±0.02'),('顶面圆定位孔','Φ16.65 +0.015/0'),('圆定位孔深（从顶面）','0.85±0.02'),('单个粘磁平面有效宽','≥3.20，平面度≤0.015'),('相对A外圆全跳动','≤0.02'),('内角圆角 / 其他锐边','R≤0.10 / C0.03~0.05')],notes=[
 'A为顶部Φ16.65孔轴线，B为顶端面，C为法线沿+X的磁铁安装面。',
 '内孔十四面贯通，顶部另车圆孔。各平面须平行A，平行度≤0.02；B对A垂直度≤0.02。',
 '磁铁粘接区Ra≤1.6；定位孔Ra≤0.8。未注线性±0.05、角度±0.5°。',
 '建议DT4C电工纯铁；可另报低碳磁性钢方案并提供牌号/B-H曲线，不得默认换不锈钢。',
 '慢走丝内孔；磁铁粘接面不得涂油或厚镀层。去油后真空/防锈包装。若热处理须在终加工前完成。',
 '最薄壁名义约0.494；顶部圆孔处0.375。成品不得局部减薄至0.35以下。']),
'P03':dict(od='Φ17.38±0.01',height='1.50±0.02',scale=5,rows=[
 ('盖板外径','17.38±0.01'),('圆定位外径 / 内径','16.60±0.01 / 14.80±0.03'),('总高 / 主盖板厚','1.50±0.02 / 0.77±0.02'),('定位裙边伸出量','0.73±0.02，从盖板底面量'),('轴胶接通孔','Φ2.03 +0.01/0'),('内圈接触凸台','Φ2.60±0.01，突出盖板底面0.20±0.01'),('底面避空环槽','外Φ6.40±0.03 / 内Φ2.80±0.03'),('避空深度','0.22±0.02，从盖板底面量'),('定位外圆相对轴孔跳动','≤0.02'),('未注锐边 / 表面','C0.03~0.05 / Ra≤1.6')],notes=[
 'A为Φ2.03轴孔轴线，B为盖板顶面；内圈凸台端面对A垂直度≤0.01。',
 '图为轴向剖面，底部环槽必须加工；其作用是避开固定轴承座与挡环。',
 '不阳极氧化。定位裙边与钢轭胶接，名义径向胶隙0.025。',
 '轴孔为胶接间隙，不是轴承配合孔。先调总成轴向游隙，再固定端盖与轴。',
 '盖板底面相对钢轭顶面名义留0.03胶层；总高最终受A01装配检验控制。',
 '未注线性±0.05、角度±0.5°；禁止用压装端盖产生未知轴承预紧。']),
'P04':dict(od='Φ2.60±0.01',height='6.25±0.02',scale=10,rows=[
 ('轴承轴径','Φ2 -0.003/-0.006（1.994~1.997）'),('肩部直径','Φ2.60±0.01'),('轴承段长度','5.95±0.02'),('肩部厚度','0.30±0.01'),('总长','6.25±0.02'),('肩根圆角','R≤0.05；轴端C0.05'),('两轴承段共轴全跳动','≤0.005'),('肩面对轴线垂直度','≤0.01'),('轴承段 / 其他表面','Ra≤0.4 / Ra≤1.6')],notes=[
 'A为共同轴承轴线，B为轴肩上端面。禁止中心架压伤配合面。',
 '轴承须先测内径分组：选配后的直径间隙目标0.002~0.006。若本尺寸带不能配合实际轴承，报价列明需调整的轴径，不能强压。',
 '316L车削后精磨；不得替换为马氏体不锈钢或普通碳钢。',
 '肩部下端面粘接C03测角磁铁，胶层目标0.02±0.01。',
 '同批加工尺寸与轴承实测内径同时记录；未注线性±0.03、角度±0.5°。']),
'P05':dict(od='Φ14.00±0.03',height='8.05±0.02',scale=5,level=.5,rows=[
 ('底座外径 / 厚度','Φ14.00±0.03 / 0.60±0.02'),('立柱外径 / 总高','Φ6.00 0/-0.01 / 8.05±0.02'),('轴承座孔 D2','Φ5.00 +0.008/+0.003'),('轴承下挡肩 Z','2.70±0.01，自底面量'),('挡肩内孔 / 轴向厚度','Φ4.50 +0.02/0 / 0.20±0.02'),('下腔孔 / 腔顶Z','Φ5.00 +0.03/0 / 2.50±0.02'),('底面传感器凹座','Φ5.60 +0.03/0，深0.40±0.02'),('轴承孔圆度 / 圆柱度','≤0.003 / ≤0.005'),('Φ6立柱相对轴承孔跳动','≤0.015'),('螺孔、通道、凸台','详见本件第2页，不得漏加工')],notes=[
 'A为Φ5轴承座孔轴线，B为底面；剖面Z尺寸全部从B起算。',
 'B对A垂直度≤0.02，下挡肩面对A垂直度≤0.01。轴承孔Ra≤0.4，挡肩Ra≤0.8。',
 '轴承座孔从顶端精镗到Z2.70；下方Φ4.50挡肩保留0.20厚，禁止贯通切掉。',
 '轴承外径实测分组，目标直径间隙0.003~0.012；不做强过盈。',
 '定子内孔Φ6.10与立柱胶接，名义径向胶层0.05；装配用心轴校正同轴。',
 '不阳极氧化。未注线性±0.05、角度±0.5°；其他表面Ra≤1.6。']),
'P06':dict(od='Φ13.00±0.02',height='0.20名义',scale=7,rows=[
 ('齿顶外径 / 中心孔','Φ13.00±0.02 / Φ6.10 +0.02/0'),('铁芯内轭外圆','Φ8.00；轮廓±0.02'),('齿靴根圆','R6.10；轮廓±0.02'),('齿身宽','0.80±0.02'),('齿靴半角 / 齿距','11.00° / 30.00°，12等分'),('名义单片厚度','0.20；实际含涂层厚度须回报'),('成叠尺寸控制','4.00±0.03，片数按实测厚度选配'),('切边毛刺 / 叠片错位','≤0.010 / ≤0.02'),('未注平面轮廓公差','±0.02，以原点及+X齿中心定位')],notes=[
 '二维切割轮廓见同号cut-profile-1to1.dxf：真实圆弧和直线，单位mm；不得缩放或重新描图。',
 '无取向电工钢，双面绝缘。供应商提供牌号、实际厚度、涂层体系、B-H与损耗数据。',
 '20片/台仅为0.20名义厚度的参考数量；报价须保证成叠4.00±0.03，不得磨掉片间绝缘凑高度。',
 '激光热影响区及毛刺须处理；慢走丝方案须说明去重铸层与重做边缘绝缘方法。',
 '不得用实心铁件替代叠片，不焊接短路各片。可报价散片与粘叠成品两档。',
 'A为中心孔轴线，B为端面，C为+X齿中心线；外圆相对A全跳动≤0.03。']),
'P07':dict(od='Φ2.60±0.01',height='t见表',scale=12,rows=[
 ('外径 / 内孔','Φ2.60±0.01 / Φ2.05 +0.02/0'),('名义厚度','0.100±0.003'),('备选厚度','0.080 / 0.090 / 0.110 / 0.120，各±0.003'),('平面度 / 毛刺','≤0.005 / ≤0.003'),('单台装用 / 备件','选用1片；首批每种厚度每台多备2片')],notes=[
 '只接触轴承内圈，不能碰防尘盖或外圈。材料316L，平整无卷曲。',
 '各厚度按独立小袋分装标识，不用在薄片上冲印厚度。',
 '与外圈隔垫配合选厚，再通过端盖位置调整总成轴向游隙0.02~0.05。',
 '不可用拧紧或压装抵消尺寸误差；图中厚度放大仅便于识别，尺寸以表为准。']),
'P08':dict(od='Φ4.98 0/-0.02',height='0.100±0.003',scale=10,rows=[
 ('外径','Φ4.98 0/-0.02'),('内孔','Φ4.50 +0.02/0'),('厚度','0.100±0.003'),('平面度 / 毛刺','≤0.005 / ≤0.003')],notes=[
 '两只轴承外圈之间的隔垫，只接触外圈挡肩面。',
 '材料316L，蚀刻或精密薄片切割；去毛刺不得造成锥面和卷曲。',
 '厚度需实测记录；与P07内圈隔垫共同选配，不通过胶厚补偿本件厚度。',
 '尺寸为成品清洁状态；独立平放包装。']),
'P09':dict(od='Φ4.98 0/-0.02',height='0.20±0.01',scale=10,rows=[
 ('外径 / 内孔','Φ4.98 0/-0.02 / Φ4.50 +0.02/0'),('厚度','0.20±0.01'),('平面度 / 毛刺','≤0.01 / ≤0.005'),('装配底面Z（参考）','7.82；由实际轴承端面调节')],notes=[
 '上轴承外圈挡环，安装于固定座Φ5孔内。',
 '相对上轴承外圈端面保留0.01~0.03轴向间隙；在外圆局部限量点胶固定。',
 '不得将胶滴入轴承；工装支承外圈，不能通过内圈或滚珠施压。',
 '与旋转端盖留避空，不可把挡环粘到转轴或端盖。']),
'A02':dict(od='Φ13.00±0.03',height='铁芯4.00±0.03',scale=7,rows=[
 ('槽极 / 连接','12槽 / 14极，三相星形'),('成叠外径 / 内孔','Φ13.00±0.03 / Φ6.10 +0.02/0'),('成叠厚度','4.00±0.03，含绝缘与叠片胶'),('每齿 / 每相串联匝数','32 / 128；每相4齿线圈'),('裸铜线径 / 成品外径','Φ0.13 / ≤Φ0.145'),('单齿参考排线','3层：11 / 11 / 10'),('绕组最大轴向包络','5.16；铁芯上下各允许0.58'),('相引出线','U/V/W各50±5，绝缘后外径≤0.30'),('20℃线间电阻','约4.29Ω仅参考；三组实测差≤5%')],notes=[
 '本STEP仅铁芯与总成装配定位参考；绕线包络在A01总装中，不是实心铜或制造模具。',
 '芯片切边加绝缘保护，薄层目标0.025~0.05；避免占满齿侧绕线空间。',
 '从+Z向下看，+X为齿1，逆时针齿1~12。齿序及绕向见下一页，不能自行改槽极/绕向。',
 '叠片需对齐，齿顶外圆相对中心孔全跳动≤0.03；不得打磨裸露短接全部叠片。',
 '100V DC绕组对铁芯绝缘电阻≥20MΩ，测试时断开电子板；记录三组电阻与匝数检查结果。',
 '相线过固定座Φ0.90孔，星点与接头绝缘、固定，均不得超出A01包络。']),
'C01':dict(od='切向3.00±0.05',height='轴向4.00±0.05',scale=12,rows=[
 ('轴向 / 切向 / 径向','4.00±0.05 / 3.00±0.05 / 1.00±0.05'),('材料','用户自备N52，牌号以供货资料为准'),('充磁方向','沿1mm厚度，两片4×3大面分别N/S'),('装配数量 / 极性','14片，朝内7N+7S，交替排列'),('来料检测','每片实测三向尺寸、极性、外观并编号'),('背面胶层','目标0.05，允许0.03~0.07'),('片间最小实体距离','装配验收≥0.03')],notes=[
 '本件是采购参考图，不外发定制，不对现货磨削、钻孔或切割。',
 '红蓝仅示意磁极；保留原镀层。缺角、剥镀、裂纹和尺寸超出本表的磁铁不得直接装配。',
 '用J01工装与PET垫片补偿厚度。每面垫厚s=1.05-实测磁铁厚度，范围0~0.10，名义0.05。',
 '先干装确认14片的间隙及中心位置，再按胶种要求粘接；胶不能占用转动气隙。',
 '普通N52不是N52H/SH；无本批退磁曲线时先短时限流，磁铁附近50℃作为初始停测点。',
 '不得把材料剩磁当成电机气隙磁密；本版未完成退磁与电磁有限元验证。']),
'C03':dict(od='Φ4.00±0.03',height='0.50±0.03',scale=12,rows=[
 ('外径 / 厚度','Φ4.00±0.03 / 0.50±0.03'),('材料','N35SH；现货替代牌号需单列'),('充磁','沿直径方向两极充磁，非轴向'),('镀层','Ni-Cu-Ni；尺寸为镀后成品尺寸'),('端面平行度','≤0.02；边缘轻倒钝≤0.05'),('安装轴向胶层','0.02±0.01，中心偏移≤0.05')],notes=[
 '测角专用两极磁铁，与C01电机磁铁功能和充磁方向不同。',
 '优先现货；无法供货时单独报定制费、MOQ、交期与可实现公差。',
 '提供牌号及充磁方向证明；极性方向用包装示意标识，不在粘接面厚涂油漆。',
 'N35SH材料指标以原厂批次数据为准；替代牌号不得默认等同。',
 '与MT6701芯片顶面名义间距0.63；本小尺寸磁铁及周边钢件的20~100mT工作场尚需实测，尺寸合格不代表测角通过。']),
'J01':dict(od='Φ22.00±0.05',height='7.10±0.02',scale=4,level=4,rows=[
 ('底座外径 / 厚度','Φ22.00±0.05 / 2.00±0.02'),('钢轭外圆定位孔 / 深','Φ17.41 +0.01/0 / 1.00±0.02'),('下定位台十四面对边','15.60±0.03'),('下台高度（相对底座顶）','1.10±0.01，支承磁铁下端'),('磁铁定位芯十四面对边','13.800±0.010'),('定位芯高度','4.00±0.02'),('中间贯通孔','Φ6.00±0.05'),('定位芯相对外定位孔偏心','≤0.005，十四面位置角±0.05°'),('补偿PET片','4×3；厚0~0.10，0.01步进，实测选配')],notes=[
 'A为Φ17.41定位孔轴线，B为底座顶面；定位芯14面与钢轭14面逐一对正。',
 '钢轭底面放在B上；磁铁底面支承在1.10高台上，对应电机全局Z=2.30。',
 '定位芯每面标中心线（浅刻≤0.02）；磁铁切向中心偏差≤0.015。',
 '每片厚度t实测，补偿片s=1.05-t；名义1.00厚磁铁用0.05垫片。PET与脱模层总厚计入s。',
 '先以实物检查0.03~0.07背胶隙；定位孔过紧时修工装，不压坏钢轭或磁铁。',
 '20±2℃使用。胶固化后轴向脱模，取净全部PET和脱模残留；不得留在工作气隙。']),
'J02':dict(od='40.00±0.10',height='4.00±0.03',scale=3,level=-3,rows=[
 ('板长×板宽×厚','40.00±0.10 ×30.00±0.10 ×4.00±0.03'),('中心通孔','Φ8.00 +0.05/0'),('电机固定孔','3×Φ1.60 +0.05/0，PCD10.80'),('三孔角度','30° / 150° / 270°，相对+X'),('台架固定孔','4×Φ3.40 +0.10/0，X=±16，Y=±11'),('顶面出线槽','宽2.00 +0.10/0，深0.45±0.03'),('出线槽方向','从中心沿120°贯通至外边缘'),('孔中心位置误差','各X/Y±0.03'),('顶面平面度 / 锐边','≤0.03 / C0.20±0.10')],notes=[
 'XY原点为板中心，Z=0为安装顶面；STEP以板底为局部Z=0。',
 '3只M1.4螺钉从板底向上固定电机，建议先备M1.4×5并选垫片；实际入座0.8~1.1，不能顶盲孔底。',
 '传感器软板及引线从中心孔和顶面槽引出，不能夹在电机底面与台架板之间。',
 '本底板用于台架，不是原机壳转接件。夹持底板边缘，不能夹住转子。',
 '材料6061-T6，无表面处理；未注线性±0.10，表面Ra≤3.2。'])}

def part_page(pid):
 u=cad.UNIQUE[pid];s=SPECS[pid];sh=cad.move(u['shape'],z=-u['z0']);bb=cad.bbox(sh);height=bb[5]-bb[2];sc=s['scale']
 page(u['name']+' · 零件图',REV+'-'+pid,u['material'],str(sc)+':1',u['qty'],pid)
 text(18,251,'横截面 / 从 +Z 向下看',11,BLUE,True)
 level=s.get('level',u['z0']+height/2)-u['z0']
 if pid=='P06':level=.1
 if pid=='P04':level=.15
 if pid=='J02':level=3.8
 if pid=='C01':
  # Purchased magnet is displayed in its own rectangular axes, not motor radial coordinates.
  sh=cad.move(sh,x=-6.95,y=1.5);bb=cad.bbox(sh)
  view(sh,'xy',2,72,184,sc,1.5)
  # Vertical section through local magnet centre, translate to let the section cut it.
  secsh=cad.move(sh,y=-1.5)
  view(secsh,'xz',0,210,184,sc,2)
  dimh(72,72+sc,226,184,'径向1.00±0.05')
  dimv(184-1.5*sc,184+1.5*sc,100,84,'3.00±0.05')
  dimv(184-2*sc,184+2*sc,238,222,'4.00±0.05')
  text(206,244,'轴向剖面 / 大面为磁极面',10,BLUE)
 else:
  if pid=='J01':
   view(sh,'xy',1,83,192,sc,color=MUTED)
   view(sh,'xy',2.5,83,192,sc,color=MUTED)
  view(sh,'xy',level,83,192,sc)
  view(sh,'xz',0,215,192,sc,height/2)
  text(173,251,'A-A 轴向剖面（无剖面线）',11,BLUE,True)
  text(18,244,'叠加Z=1/2.5/4截面；灰色为下部' if pid=='J01' else f'剖切位置：零件局部 Z={level:.2f}',9,MUTED)
  # Centre lines and main dimensions, all profiles taken from the actual STEP source.
  radius=max(abs(bb[0]),abs(bb[3]))
  line(83,max(145,192-radius*sc-5),83,min(239,192+radius*sc+5),.1,MUTED,[3,1,1,1])
  line(215,192-height*sc/2-5,215,192+height*sc/2+5,.1,MUTED,[3,1,1,1])
  dimh(83+bb[0]*sc,83+bb[3]*sc,139,192,s['od'])
  if height*sc>=5:dimv(192-height*sc/2,192+height*sc/2,215+radius*sc+5,215+radius*sc,'H')
  else:
   arrow((244,214),(215+radius*sc*.7,192));text(243,217,'t：'+s['height'],9)
  if pid=='C03':arrow((65,192),(101,192));text(83,225,'直径方向 N → S',10,BLUE,align='center')
  labels={
   'P01':[((135,224),(126.6,192),'内孔17.44 +0.03/0')],
   'P02':[((127,231),(123,192),'14面：距轴8.000±0.015')],
   'P03':[((239,216),(220.075,192),'轴孔2.03 +0.01/0')],
   'P04':[((242,233),(224.975,209),'轴径2 -0.003/-0.006')],
   'P05':[((244,229),(227.5125,199),'座孔5 +0.008/+0.003')],
   'P06':[((132,223),(118,194.8),'齿宽0.80±0.02')],
   'P07':[((116,223),(95.3,192),'内孔2.05 +0.02/0')],
   'P08':[((120,224),(105.5,192),'内孔4.50 +0.02/0')],
   'P09':[((120,224),(105.5,192),'内孔4.50 +0.02/0')],
  }
  for a,b,label in labels.get(pid,[]):
   arrow(a,b);text(a[0]-4,a[1]+3,label,8.5,BLUE)
 table(16,128,[87,188],[('尺寸 / 特征','成品要求（mm；角度另注）')]+s['rows'],rowh=6,size=9)
 line(295,50,295,255,.15,MUTED);text(301,251,'基准、工艺及检验要求',11,BLUE,True)
 y=notes(301,242,s['notes'],101,9.5)
 assert y>48,(pid,y)
 end()

def carrier_detail():
 page('定子及轴承固定座 · 孔位与通道',REV+'-P05 / 2','6061-T6 铝','8:1',1,'P05')
 sh=cad.UNIQUE['P05']['shape'];view(sh,'xy',.20,100,185,8)
 text(33,251,'底座 Z=0.20 横截面 / +Z观察',11,BLUE,True)
 for a,label in [(30,'H1'),(150,'H2'),(270,'H3')]:
  x,y=5.4*math.cos(math.radians(a)),5.4*math.sin(math.radians(a))
  arrow((100+x*10,185+y*10),(100+x*8,185+y*8));text(100+x*10,185+y*10+3,label,10)
 text(33,115,'+X 向右，+Y 向上；三个螺孔从底面加工。',10)
 table(17,104,[30,55,55,55],[('孔号','X','Y','定义'),('H1','+4.67654','+2.70000','M1.4×0.3-6H'),('H2','-4.67654','+2.70000','M1.4×0.3-6H'),('H3','0.00000','-5.40000','M1.4×0.3-6H'),('相线孔','0.00000','+4.70000','Φ0.90贯穿底盘')],rowh=8,size=9)
 notes(225,250,[
 'A=轴承孔轴线，B=安装底面。孔位表为理论坐标，各孔中心X/Y误差≤±0.025。',
 '三个凸台：Φ2.40±0.05，凸台顶Z=1.60±0.03；同心于对应螺孔。',
 'M1.4×0.3-6H从底面进入，有效完整牙深1.00~1.10；底孔总深≤1.40，孔顶剩余壁厚≥0.15。STEP仅画Φ1.10底孔，不代表不攻牙。',
 '必须采用适合盲孔的底部成牙工艺；无法保证完整牙深时，报价书列出可实现深度和替代方案。',
 '传感器出线槽：宽2.00 +0.10/0，深0.40±0.02，从中心沿120°至外圆；相对原R0的135°方向转动-15°。',
 '相线孔Φ0.90 +0.05/0，从底面贯穿0.60厚底盘；孔口去毛刺C0.05，不能刮破引线。',
 '底面凹座及出线槽不得与螺孔连通；检查所有盲孔无穿顶。',
 '基准面与轴承孔应在稳定装夹中完成；提供座孔上下两处直径、圆度、立柱跳动及盲孔牙规检验记录。'],170,10)
 end()

def winding_detail():
 page('绕线定子组件 · 绕向与接线',REV+'-A02 / 2','电工钢 + 漆包铜线','齿序示意',1,'A02')
 view(cad.UNIQUE['A02']['shape'],'xy',4.3,95,185,7)
 for i,phase in enumerate(cad.SEQ):
  a=math.radians(i*30);text(95+57*math.cos(a),185+57*math.sin(a),f'{i+1} {phase}',11,BLUE,True,'center')
 text(22,251,'从 +Z 看向底面，齿1沿 +X，逆时针编号',11,BLUE,True)
 table(17,112,[28,73,35,63],[('相','串联齿序','相引出端','星点端'),('U / A','1+ → 2- → 7- → 8+','齿1起线','齿8收线'),('V / B','3- → 4+ → 9+ → 10-','齿3起线','齿10收线'),('W / C','5+ → 6- → 11- → 12+','齿5起线','齿12收线')],rowh=9,size=9)
 notes(230,250,[
 '绕向约定：沿该齿由外圆朝圆心看，从起线到收线，“+”为顺时针32匝，“-”为逆时针32匝。三个相统一使用此观察方向。',
 '每相按表中齿序连接：前一个齿的收线接下一个齿的起线。由于绕向已按正负制作，不再额外反接负号齿。',
 '三相末端（齿8、10、12的收线）接成一个绝缘星点；星点不引出。U/V/W各保留50±5引线。',
 '每齿32匝，不得用电阻相近替代匝数检查。参考3层11/11/10匝；实际排线必须满足包络。',
 '裸铜直径0.13，绝缘后最大0.145；不采用削漆绕线。齿边必须绝缘且无毛刺。',
 '相间线间电阻分别测U-V/V-W/W-U，20±2℃三组差值/平均值≤5%；约4.29Ω仅为R0解析参考。',
 '低电流确认相序、7极对与编码器电角度零位。供应商如只能提供不同绕线方案，须明确齿序、匝数与接法。',
 '若外发整套绕线定子，报价已经包含P06叠片；不得再重复计算20片的成本。'],169,10)
 end()

def render_cutaway():
 polys=[];colors=[]
 for p in cad.PARTS:
  sh=cad.cut(p['shape'],cad.box(0,-25,-1,25,25,12))
  if sh.IsNull():continue
  mesh=cad.mesh(sh)
  if not mesh['triangles']:continue
  v=np.array(mesh['vertices']);tri=np.array(mesh['triangles']);ps=v[tri]
  normal=np.cross(ps[:,1]-ps[:,0],ps[:,2]-ps[:,0]);normal/=np.maximum(np.linalg.norm(normal,axis=1)[:,None],1e-9)
  h=p['color'].lstrip('#');rgb=np.array([int(h[j:j+2],16)/255 for j in (0,2,4)])
  shade=.55+.45*np.abs(normal@np.array([.3,-.4,.866]))
  polys.extend(ps);colors.extend(shade[:,None]*rgb)
 ps=np.array(polys);az,el=math.radians(-50),math.radians(32)
 forward=np.array([math.cos(az)*math.cos(el),math.sin(az)*math.cos(el),math.sin(el)])
 right=np.array([-math.sin(az),math.cos(az),0]);up=np.cross(forward,right)
 pp=np.stack([ps@right,-ps@up,ps@forward],axis=-1)
 lo=pp[:,:,:2].min(axis=(0,1));hi=pp[:,:,:2].max(axis=(0,1));W,H=1500,1200
 scale=min((W-100)/(hi[0]-lo[0]),(H-100)/(hi[1]-lo[1]));pp[:,:,:2]=(pp[:,:,:2]-(hi+lo)/2)*scale+np.array([W/2,H/2])
 buf=np.full((H,W,3),255,dtype=np.uint8);depth=np.full((H,W),-np.inf)
 for t,col in zip(pp,colors):
  x0=max(0,int(np.floor(t[:,0].min())));x1=min(W-1,int(np.ceil(t[:,0].max())));y0=max(0,int(np.floor(t[:,1].min())));y1=min(H-1,int(np.ceil(t[:,1].max())))
  a,b,d=t;den=(b[1]-d[1])*(a[0]-d[0])+(d[0]-b[0])*(a[1]-d[1])
  if abs(den)<1e-10 or x1<x0 or y1<y0:continue
  xx,yy=np.meshgrid(np.arange(x0,x1+1)+.5,np.arange(y0,y1+1)+.5)
  u=((b[1]-d[1])*(xx-d[0])+(d[0]-b[0])*(yy-d[1]))/den
  v=((d[1]-a[1])*(xx-d[0])+(a[0]-d[0])*(yy-d[1]))/den;w=1-u-v;z=u*a[2]+v*b[2]+w*d[2]
  patch=depth[y0:y1+1,x0:x1+1];mask=(u>=-1e-8)&(v>=-1e-8)&(w>=-1e-8)&(z>patch)
  patch[mask]=z[mask];buf[y0:y1+1,x0:x1+1][mask]=np.clip(col*255,0,255).astype(np.uint8)
 Image.fromarray(buf).save(IMG/'HW18-B1-cutaway.png')

def assembly_pages():
 page('N52 方块磁铁电机 · 总装与询价范围',REV+'-A01 / 1','多材料','剖切透视非比例','1')
 c.drawImage(str(IMG/'HW18-B1-cutaway.png'),16*mm,105*mm,width=225*mm,height=145*mm,preserveAspectRatio=True,anchor='c')
 text(20,101,'由本版实体模型生成；轴承、绕组、电子器件为包络表示。',9,MUTED)
 table(17,92,[61,164],[('总成名义包络','Φ18.00 × 8.80；验收最大Φ18.05 × 9.00'),('电机拓扑','12槽 / 14极；三相星形；定子Φ13 ×4'),('主磁铁','14 × N52 4×3×1；用户自备现货'),('用途','台架验证；不是原Φ17机壳直接替换件')],rowh=8,size=9)
 notes(250,251,[
 '外发机械件：P01~P05；薄垫片/挡环P07~P09。每件均有独立STEP与本图册分册PDF。',
 '铁芯与绕线：P06是单片，A02是绕线组件。可分别询价散片、成叠铁芯、成品绕线组件，避免重复计价。',
 '采购件：C01用户自备磁铁；C02两只MR52ZZ轴承；C03独立两极测角磁铁（现货或单列定制）。',
 '辅助加工：J01粘磁工装每批1套；J02台架底板可每台1件。不含原机壳适配支架。',
 '本包只发布机械及绕线询价资料。MT6701软板、驱动板、线束与固件沿既有电子设计另行准备；本STEP的电子件不能当作PCB制造文件。',
 '报价建议按1 / 5 / 10台分别列单价、工装费、材料、检测费、交期与税运费。任何尺寸或材料偏离均逐项填写。',
 '这是几何检查后的样机询价版；没有完成电磁有限元、温升、退磁裕量或寿命验证，不能承诺1.6mN·m持续额定力矩。'],154,10)
 end()
 page('电机总装 · 径向与轴向剖面',REV+'-A01 / 2','多材料','剖面 7:1','1')
 for p in cad.PARTS:
  view(p['shape'],'xy',4.3,90,186,7,color=HexColor(p['color']))
  view(p['shape'],'xz',0,278,190,7,4.4,color=HexColor(p['color']))
 text(20,251,'径向剖面 Z=4.30',11,BLUE,True);text(213,251,'轴向剖面 Y=0',11,BLUE,True)
 dimh(27,153,114,186,'Φ18.00 名义 / 18.05 MAX')
 dimv(159.2,220.8,350,341,'8.80名义')
 table(17,100,[60,78,62,167],[('部位','全局Z范围','部位','全局Z范围'),('固定座底 / 顶','0.00 / 8.05','钢轭底 / 顶','1.20 / 8.00'),('磁铁 / 铁芯','2.30~6.30','上下轴承','2.70~5.20 / 5.30~7.80'),('内外隔垫','5.20~5.30','上挡环（参考）','7.82~8.02'),('测角磁铁','1.88~2.38','MT6701最大顶部','1.25；名义空气间隙0.63'),('端盖主底 / 顶','8.03 / 8.80','转轴肩面 / 顶','2.70 / 8.65')],rowh=7,size=9)
 text(207,136,'最小名义磁气隙0.45；磁铁边缘径向约0.61。',10,BLUE)
 text(207,129,'整机必须全周无擦碰；实测最小气隙≥0.25。',10)
 text(207,122,'转子轴向游隙0.02~0.05；装配校准后粘固端盖。',10)
 end()

def requirements():
 page('加工与检验要求 · 通用条款',REV+'-REQ / 1','所有外发件','不适用','-')
 notes(18,250,[
 '文件优先级：本版PDF明确尺寸/公差/材料优先；STEP提供名义实体与未逐一标注的形状；切割DXF提供1:1名义平面轮廓。文件冲突或工艺无法实现时，供应商必须在报价中列明，不可自行平均或放宽。',
 '未注公差采用各零件页的明确规定。未给统一ISO公差等级，避免小尺寸被粗糙公差覆盖。所有尺寸单位mm；检验温度20±2℃；尺寸为成品、清洁且含表面处理状态。',
 '关键尺寸100%检验：P02十四面位置/角度及定位孔；P04轴径；P05轴承孔/下挡肩/盲孔；P06与A02外形、叠厚；P07/P08厚度。首件提供全尺寸报告，每批提供材料证明及关键尺寸实测表。',
 '轴承与磁铁须实测后分组。C01每片测轴向/宽/厚、标极性；C02测内径、外径并索取精度与游隙分组。轴/孔的图纸尺寸用于报价，最终必须同时满足装配间隙及总成检验要求。',
 '所有切边去毛刺，绝缘线经过处不能留尖边。图中一般轻倒钝及螺纹牙型未建入STEP，以二维要求实施；除此之外不得任意加大倒角、圆角或刀具让位。',
 '胶接面清洗、干燥；报告胶种、底涂、胶层控制与固化工艺。N52磁铁装入后避免高温固化，优先室温固化；不得用热压/回流工艺处理已装磁铁的总成。',
 '钢轭磁性材料、定子电工钢和两种磁铁不得混用。材料替代单独列BOM、磁性能与报价；任何电磁性能数字必须注明是材料数据、仿真还是实测。',
 '薄壁钢轭、POM外圈及薄垫片独立支撑包装。磁铁隔离包装并标极性；轴承保持原包装清洁，避免磨粒及胶污染。'],183,10)
 notes(219,250,[
 '建议报价分包：A 车铣/磨削件P01~P05+J01/J02；B 精密垫片P07~P09；C 电工钢P06及叠片绕线A02；D 轴承C02和测角磁铁C03。',
 '一台成品用料：P01~P05各1，P07~P09各1，A02一套，C01×14、C02×2、C03×1。P06由A02包含，名义20片。P07另备选厚；J01每批共用。',
 '报价表每项至少列：数量阶梯1/5/10、材料牌号、加工工艺、可达公差、单价、一次性工装费、交期、检测报告、税运费。',
 '供应商不能以“按3D加工”代替轴承配合、螺纹、表面与公差要求；不能以“可装配”代替检测记录。',
 '关键未验证事项：气隙磁密、力矩常数、齿槽转矩、持续堵转温升、N52退磁裕量、独立编码磁铁场强与电机串扰。这些由样机测试解决，不包装成已经达标。',
 '台架最大外包络Φ18.05×9.00；底板、相线、外部驱动电路与固定螺钉不计入电机本体包络。原Φ17整机包络已明确放宽，仅适用于此台架版本。'],183,10)
 end()
 page('公差链与装配验收',REV+'-REQ / 2','整机及配合','不适用','1')
 table(17,249,[79,35,70],[('径向气隙预算项','单边mm','说明'),('名义最小气隙','0.450','磁铁面中心处'),('钢轭平面径向位置','0.015','每平面分别控制'),('背胶层变化','0.020','0.03~0.07'),('磁铁厚度来料偏差','0.050','现货允许±0.05'),('定子半径偏差','0.015','成叠外径±0.03'),('装配偏心','0.030','转子轴对定子中心'),('轴承径向运动预算','0.020','含配合及内部游隙'),('热/胶层变化预留','0.020','暂定设计预算'),('合计消耗 / 剩余','0.170 / 0.280','均为预算，需实测'),('总成验收下限','0.250','所有角度且轻推侧向检查')],rowh=9,size=9)
 notes(218,250,[
 '磁铁装配：内表面中心名义半径6.95，邻片名义最小间距0.168。按来料宽±0.05、厚±0.05及位置预算，最差邻片间距约0.036；成品要求≥0.03。',
 '轴承轴选配目标直径间隙0.002~0.006；轴承外圈与座孔目标直径间隙0.003~0.012。若采购轴承尺寸组不能满足，供应商单列选配/终磨方案。',
 '不能跨越14个平面直接读表判断磁铁环跳动。逐片测面中心到钢轭A轴距离，应在6.865~7.035；装配后拟合转子轴对定子中心偏心≤0.03，并实测最小气隙。',
 '总成轴向游隙0.02~0.05，用±0.2N轴向轻载切换测位移；记录测法。调P07和端盖位置，不把端盖压死。',
 'POM外圆对转轴总跳动≤0.08；端盖端面总跳动≤0.05。全周手转无擦碰、无周期性机械卡滞；磁性齿槽感不能当作机械擦碰。',
 '测角磁铁相对轴心偏移≤0.05，端面轴向跳动≤0.05；芯片最大顶部至磁铁最小空气间隙≥0.50。装配后验证实际场强和角度连续性。',
 '几何检验不替代磁性能测试。相电流从小到大、短时验证力矩，测三组电阻、反电动势一致性、温升及编码角误差。暂用磁铁附近50℃停测，非额定温度认证。'],183,10)
 text(18,127,'此表采用各项最不利方向算术叠加；不是统计公差或实际测量结果。',9,MUTED)
 end()
 page('装配顺序与试验记录',REV+'-REQ / 3','整机','不适用','1')
 notes(18,250,[
 '来料：检查磁铁4×3×1成品尺寸并编号，筛掉裂纹/掉镀/尺寸超差；确认两大面为N/S。轴承内外径分组；先干装关键配合。',
 '绕线：按A02完成叠片、边缘绝缘、绕线和星点连接；检验匝数、电阻平衡与对芯绝缘。',
 '固定部分：定子内孔与P05立柱用0.05目标胶层粘接，用同轴工装定位，铁芯底面Z=2.30±0.03；相线穿指定孔并做防磨与拉力固定。',
 '轴系：P04从固定座下方插入，下轴承从上方套轴并落到Z2.70挡肩；安装P07/P08两种隔垫，再装上轴承。力只施加到正在配合的轴承圈。',
 '挡环：P09在上轴承外圈上方保留0.01~0.03间隙，外圆少量点胶定位，彻底防止胶进入滚道。',
 '转子粘磁：钢轭放入J01，按每片实测厚度放PET补偿片，14个面按N/S交替、切向居中粘接。名义胶层0.05；磁铁下沿贴工装台阶。',
 '胶固化后轴向脱模，取净PET。逐片复查极性、宽缝和内表面半径；不得磨磁铁来消除碰擦。',
 '总装：转子钢轭从外侧套入定子；装P03，调轴向游隙及同轴度后固定端盖/钢轭/轴。端盖下裙边不能碰线圈，避空环槽不能漏做。'],184,10)
 notes(220,250,[
 '安装POM外圈，使用POM适配底涂/胶，控制外圆跳动与最高点高度。',
 'C03测角磁铁粘在轴肩下端，名义胶层0.02；安装传感器软板，尾部沿120°出线。相对R0板整体旋转-15°安装。',
 '放到J02台架板，检查软板和导线没有被压住；M1.4螺钉入孔0.8~1.1，使用低扭矩并避免顶底。',
 '断电检查：全周气隙≥0.25、轴向游隙0.02~0.05、最小磁铁间距≥0.03、磁铁到芯片≥0.50；记录转动阻力、跳动和是否擦碰。',
 '低电流试验：先校验7极对、相序、编码方向和电角度零位；逐步验证位置保持、虚拟档位、阻尼。每次记录相电流、力矩/轮缘力、温度、持续时间。',
 '持续保持试验另行递增时间。记录冷却后力矩/反电动势是否恢复，用于排查不可逆退磁。不能因为短时能转就宣称持续工作通过。',
 '样机记录表至少含：磁铁编号与尺寸/极性、轴承组、轴径/孔径、隔垫厚度、胶种/固化、绕组电阻、气隙/游隙/跳动、测试电流/温度/力矩。'],183,10)
 end()
 page('依据、文件目录与版本说明',REV+'-REQ / 4','工程依据','不适用','-')
 notes(18,250,[
 '沿用R0：定子Φ13/内孔Φ6.1/叠厚4，12槽14极，32匝/齿，Φ0.13铜线，MR52ZZ双轴承，MT6701与独立两极测角磁铁。',
 '变更B1：磁瓦改用户自备N52方块；十四平面钢轭；外径18、气隙0.45；钢轭顶部圆定位孔及新端盖；轴承座下挡肩+独立外隔垫；传感器出线改120°；增加工装与台架板。',
 'STEP/：各零件以局部底面为Z=0；A01总装使用全局坐标，旋转轴为Z；P05底面为全局Z=0。C01单件仍保留径向XY位置，尺寸见其PDF。',
 'PDF/：逐件图纸分册及本总图册；P05和A02各两页。DXF/：1:1切割/工艺轮廓，文件名标明cut-profile或profile；除明确切割轮廓外，不含刀补与机床工艺。',
 'BOM_询价清单.csv、供应商询价文字.txt、检验记录模板.csv：可随对应分包资料发送。J01与J02为辅助件；C01不计定制磁铁采购价。',
 'verification/：实体与STEP回读、零件干涉、转动扫掠、公差预算、DXF轮廓和PDF排版检查记录。通过这些检查不代表电磁性能已验证。'],184,10)
 text(220,250,'核查的原厂资料与工程参考',12,BLUE,True)
 refs=[
 ('EZO MR52ZZ：2×5×2.5，轴肩2.6 / 孔肩4.5，角半径0.1。','https://www.ezo-brg.co.jp/english/product/spec.php?eid=00077'),
 ('NHBB：微型轴承配合、径向游隙与装配力施加原则。','https://www.nhbb.com/knowledge-center/engineering-reference/miniature-instrument-bearings/recommended-fits'),
 ('Arnold：N52与N35SH材料指标、耐温及退磁性能差异。','https://www.arnoldmagnetics.com/products/neodymium-iron-boron-magnets/'),
 ('K&J：块状磁铁充磁方向及厚度定义。','https://www.kjmagnetics.com/magdir.asp'),
 ('MagnTek MT6701 Rev1.8：传感器磁场要求与器件尺寸。','https://www.magntek.com.cn/upload/pdf/202407/MT6701_Rev.1.8.pdf')]
 y=239
 for title,url in refs:
  y=wrapped(220,y,title,183,10,5)
  y=wrapped(220,y,url,183,8,4)-8
 text(220,y-2,'资料核查日期：2026-10-30；供应商仍须提供本批次数据。',9,MUTED)
 end()

if __name__=='__main__':
 cad.build();print('Rendering actual CAD cutaway...',flush=True);render_cutaway()
 assembly_pages()
 for pid in ['P01','P02','P03','P04','P05','P06','P07','P08','P09','A02','C01','C03','J01','J02']:
  part_page(pid)
  if pid=='P05':carrier_detail()
  if pid=='A02':winding_detail()
 requirements();c.save()
 reader=PdfReader(PDF);assert len(reader.pages)==PAGE
 for pid,pages in PART_PAGE.items():
  w=PdfWriter()
  for p in pages:w.add_page(reader.pages[p])
  with (OUT/f'{REV}-{pid}.pdf').open('wb') as f:w.write(f)
 shutil.copy2(PDF,OUT/PDF.name)
 outliers=[t for t in TEXT if t['bbox'][0]<10 or t['bbox'][2]>410 or t['bbox'][1]<10 or t['bbox'][3]>287]
 check={'pages':PAGE,'part_pages':PART_PAGE,'page_index':PAGES,'text_count':len(TEXT),'out_of_border_text':outliers,'all_pages_have_text':all(bool(p.extract_text()) for p in reader.pages),'notes':'Visual rendering review is required in addition to this text bounds check.'}
 (cad.QA/'pdf-check.json').write_text(json.dumps(check,ensure_ascii=False,indent=2))
 (cad.QA/'text-boxes.json').write_text(json.dumps(TEXT,ensure_ascii=False,indent=2))
 assert not outliers,outliers
 print(f'Created {PAGE} pages at {PDF}',flush=True)
