"""Create vector dimension drawings and review illustrations for HW17-R0."""
import os
os.environ.setdefault('MPLCONFIGDIR','/private/tmp/owt-motor-mpl')
import json, math, textwrap
from pathlib import Path
import numpy as np
from PIL import Image
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.patches import Circle, Rectangle, Wedge, Polygon
from mpl_toolkits.mplot3d.art3d import Poly3DCollection
import ezdxf
from OCP.BRepAlgoAPI import BRepAlgoAPI_Section, BRepAlgoAPI_Cut
from OCP.BRepAdaptor import BRepAdaptor_Curve
from OCP.GeomAbs import GeomAbs_Circle, GeomAbs_Line
from OCP.TopAbs import TopAbs_EDGE
from OCP.gp import gp_Pln, gp_Pnt, gp_Dir
from OCP.TopExp import TopExp_Explorer
from OCP.TopoDS import TopoDS
from reportlab.pdfgen import canvas
from reportlab.lib.colors import HexColor, Color, white
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.cidfonts import UnicodeCIDFont
import build as cad

HERE=Path(__file__).resolve().parent;ROOT=cad.ROOT
IMG=HERE/'images';IMG.mkdir(exist_ok=True)
PDFDIR=ROOT/'output/pdf';PDFDIR.mkdir(parents=True,exist_ok=True)
D=json.loads((HERE/'design.json').read_text());G=D['geometry'];E=D['electromagnetics']
A=json.loads((HERE/'analysis.json').read_text());V=json.loads((HERE/'verification.json').read_text())
pdfmetrics.registerFont(UnicodeCIDFont('STSong-Light'))
plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False,'savefig.facecolor':'white'})
cad.build()

def render3d(name,mode):
 # Per-pixel depth buffer avoids painter-order artifacts between nested CAD
 # surfaces and long coplanar section triangles. Illustration uses real B-rep mesh.
 allpolys=[];allcolors=[]
 for p in cad.PARTS:
  sh=p['shape'];offset=0
  if mode=='cutaway':sh=cad.cut(sh,cad.box(0,-20,-1,20,20,12))
  if mode=='exploded':
   offset={'01_grip_ring':24,'02_rotor_back_iron':16,'03_rotor_cap':25,'04_rotor_shaft':0,'13_sensor_magnet':-2,'14_sensor_flex':-5,'15_MT6701_max_envelope':-5,'16_sensor_decoupling':-5}.get(p['name'],0)
   if p['group']=='magnet':offset=10
  if sh.IsNull():continue
  verts,tri=cad.mesh(sh)
  if not verts:continue
  vv=np.array(verts);vv[:,2]+=offset;tt=np.array(tri)
  polys=vv[tt]
  norms=np.cross(polys[:,1]-polys[:,0],polys[:,2]-polys[:,0]);norms/=np.maximum(np.linalg.norm(norms,axis=1)[:,None],1e-8)
  rgb=np.array(matplotlib.colors.to_rgb(p['color']));shade=.58+.42*np.abs(norms@np.array([.3,-.4,.866]))
  allpolys.extend(polys);allcolors.extend(shade[:,None]*rgb)
 polys=np.array(allpolys);colors=np.array(allcolors)
 el=math.radians(24 if mode=='exploded' else 30);az=math.radians(-55)
 forward=np.array([math.cos(az)*math.cos(el),math.sin(az)*math.cos(el),math.sin(el)])
 right=np.array([-math.sin(az),math.cos(az),0]);up=np.cross(forward,right)
 proj=np.stack([polys@right,-polys@up,polys@forward],axis=-1)
 lo=proj[:,:,:2].min(axis=(0,1));hi=proj[:,:,:2].max(axis=(0,1))
 W,H=1800,1620;scale=min((W-220)/(hi[0]-lo[0]),(H-180)/(hi[1]-lo[1]))
 proj[:,:,:2]=(proj[:,:,:2]-(hi+lo)/2)*scale+np.array([W/2,H/2])
 buf=np.full((H,W,3),255,dtype=np.uint8);depth=np.full((H,W),-np.inf)
 for tri,col in zip(proj,colors):
  x0=max(0,int(np.floor(tri[:,0].min())));x1=min(W-1,int(np.ceil(tri[:,0].max())))
  y0=max(0,int(np.floor(tri[:,1].min())));y1=min(H-1,int(np.ceil(tri[:,1].max())))
  a,b,c=tri;den=(b[1]-c[1])*(a[0]-c[0])+(c[0]-b[0])*(a[1]-c[1])
  if abs(den)<1e-10 or x1<x0 or y1<y0:continue
  xx,yy=np.meshgrid(np.arange(x0,x1+1)+.5,np.arange(y0,y1+1)+.5)
  u=((b[1]-c[1])*(xx-c[0])+(c[0]-b[0])*(yy-c[1]))/den
  v=((c[1]-a[1])*(xx-c[0])+(a[0]-c[0])*(yy-c[1]))/den;w=1-u-v
  z=u*a[2]+v*b[2]+w*c[2];patch=depth[y0:y1+1,x0:x1+1]
  mask=(u>=-1e-8)&(v>=-1e-8)&(w>=-1e-8)&(z>patch)
  patch[mask]=z[mask];buf[y0:y1+1,x0:x1+1][mask]=np.clip(col*255,0,255).astype(np.uint8)
 Image.fromarray(buf).resize((1200,1080),Image.Resampling.LANCZOS).save(IMG/(name+'.png'))

def dim(ax,p,q,label,off=(0,0),fs=9):
 p=np.array(p);q=np.array(q);d=np.array(off);pp=p+d;qq=q+d
 ax.plot([p[0],pp[0]],[p[1],pp[1]],color='#8996a2',lw=.6);ax.plot([q[0],qq[0]],[q[1],qq[1]],color='#8996a2',lw=.6)
 ax.annotate('',xy=pp,xytext=qq,arrowprops=dict(arrowstyle='<->',lw=.8,color='#263b4b',shrinkA=0,shrinkB=0))
 mid=(pp+qq)/2
 ax.text(mid[0]+(.15 if abs(qq[0]-pp[0])<.01 else 0),mid[1]+(.25 if abs(qq[1]-pp[1])<.01 else 0),label,ha='left' if abs(qq[0]-pp[0])<.01 else 'center',va='center',fontsize=fs,rotation=90 if abs(qq[0]-pp[0])<.01 else 0,bbox=dict(fc='white',ec='none',pad=.8))

def section():
 fig,ax=plt.subplots(figsize=(13,7.4),dpi=180)
 def rect(x,z,w,h,c,hatch=None):ax.add_patch(Rectangle((x,z),w,h,fc=c,ec='#42515a',lw=.45,hatch=hatch))
 for s in [-1,1]:
  def rr(r0,r1,z0,z1,c):rect(r0 if s==1 else -r1,z0,r1-r0,z1-z0,c)
  rr(8.2,8.5,1.2,8.8,'#667683');rr(7.7,8.2,1.2,8,'#9fadb7');rr(6.75,7.65,2.05,6.55,'#d96159' if s==1 else '#4c8bbd')
  rr(1,8.2,8.1,8.8,'#d3dce1');rr(3.2,8.2,8,8.1,'#d3dce1');rr(1,1.3,7.8,8.1,'#d3dce1');rr(7,7.68,7.4,8,'#d3dce1')
  rr(3.05,6.5,2.3,6.3,'#7d8c96')
  rr(4.2,5.95,1.72,2.2,'#d09350');rr(4.2,5.95,6.4,6.88,'#d09350')
  rr(2.8,7,0,.4,'#bccbd3');rr(2.5,7,.4,.6,'#bccbd3');rr(2.5,3,.6,7.8,'#bccbd3');rr(2.25,2.5,5.2,5.3,'#bccbd3')
  for z in [2.7,5.3]:rr(1,2.5,z,z+2.5,'#dde4e8')
  for z in [2.5,7.8]:rr(2.25,2.5,z,z+.2,'#a2b3be')
  rr(1,1.3,5.2,5.3,'#c1cad0')
 rect(-1,2.7,2,5.95,'#acb9c2');rect(-1.3,2.4,2.6,.3,'#acb9c2')
 rect(-2,1.88,4,.5,'#b088b6');rect(-2.75,0,5.5,.4,'#3b977d');rect(-1.55,.45,3.1,.8,'#34434d');rect(1.65,.45,.6,.3,'#a18e76')
 ax.plot([0,0],[-1,10],ls='-.',lw=.6,c='#7a8791')
 dim(ax,(-8.5,8.8),(8.5,8.8),'17.00 max OD',off=(0,1))
 dim(ax,(8.5,0),(8.5,8.8),'8.80 nominal / 9.00 max',off=(1.3,0))
 ax.annotate('Base datum Z=0\ncase Y=2.50',xy=(-6.8,0),xytext=(-15,2),fontsize=9,arrowprops=dict(arrowstyle='->',color='#263b4b'))
 ax.annotate('0.25 radial air gap',xy=(6.625,4.2),xytext=(11,5),fontsize=10,arrowprops=dict(arrowstyle='->',color='#263b4b'))
 ax.annotate('0.63 sensor top-to-magnet gap',xy=(0,1.55),xytext=(-4,-1.1),fontsize=10,arrowprops=dict(arrowstyle='->',color='#263b4b'))
 ax.annotate('2 x MR52ZZ\n2 x 5 x 2.5 mm',xy=(-1.8,4),xytext=(-15,5.5),fontsize=10,arrowprops=dict(arrowstyle='->',color='#263b4b'))
 ax.annotate('4.00 active stack',xy=(-4,4.3),xytext=(-15,7.6),fontsize=10,arrowprops=dict(arrowstyle='->',color='#263b4b'))
 ax.set_aspect('equal');ax.set_xlim(-16,17);ax.set_ylim(-1.8,10.6);ax.axis('off')
 fig.tight_layout();fig.savefig(IMG/'section.png');plt.close(fig)

def stator_edges():
 core=next(p['shape'] for p in cad.PARTS if p['name']=='07_stator_stack')
 sec=BRepAlgoAPI_Section(core,gp_Pln(gp_Pnt(0,0,4.3),gp_Dir(0,0,1)),False);sec.Build();assert sec.IsDone()
 out=[];ex=TopExp_Explorer(sec.Shape(),TopAbs_EDGE)
 while ex.More():
  edge=TopoDS.Edge(ex.Current());c=BRepAdaptor_Curve(edge);a,b=c.FirstParameter(),c.LastParameter()
  if c.GetType()==GeomAbs_Line:
   p,q=c.Value(a),c.Value(b);out.append({'type':'line','a':(p.X(),p.Y()),'b':(q.X(),q.Y())})
  elif c.GetType()==GeomAbs_Circle:
   cir=c.Circle();p=cir.Location();sa=math.degrees(math.atan2(c.Value(a).Y()-p.Y(),c.Value(a).X()-p.X()));ea=math.degrees(math.atan2(c.Value(b).Y()-p.Y(),c.Value(b).X()-p.X()))
   if cir.Axis().Direction().Z()<0:sa,ea=ea,sa
   out.append({'type':'circle' if b-a>2*math.pi-1e-6 else 'arc','center':(p.X(),p.Y()),'r':cir.Radius(),'start':sa,'end':ea})
  else:raise ValueError('Unexpected stator contour')
  ex.Next()
 return out

def winding_and_dxf():
 edges=stator_edges();doc=ezdxf.new('R2010');doc.units=4;m=doc.modelspace()
 doc.layers.new('CUT_PROFILE',dxfattribs={'color':7});doc.layers.new('REFERENCE',dxfattribs={'color':3});doc.layers.new('NOTES',dxfattribs={'color':2})
 for e in edges:
  attrs={'layer':'CUT_PROFILE'}
  if e['type']=='line':m.add_line(e['a'],e['b'],dxfattribs=attrs)
  elif e['type']=='circle':m.add_circle(e['center'],e['r'],dxfattribs=attrs)
  else:m.add_arc(e['center'],e['r'],e['start'],e['end'],dxfattribs=attrs)
 for i,txt in enumerate(['HW17-R0 STATOR LAMINATION - REVIEW ONLY','Units mm; geometry 1:1. 12 teeth. OD13 / ID6.10.','20 x nominal 0.20 insulated electrical steel; finished stack 4.00.','Tooth neck 0.80; tip span 22 deg; radial tip depth 0.40.','Prototype laser/EDM: deburr, edge insulation, stack factor/B-H review.','Do NOT laser-cut from a solid 4 mm plate. No cut compensation included.']):
  m.add_text(txt,dxfattribs={'height':.65,'insert':(-10,-9-i*1.15),'layer':'NOTES'})
 doc.saveas(HERE/'HW17-R0-stator-lamination.dxf');audit=doc.audit();assert not audit.errors
 fig,ax=plt.subplots(figsize=(9,9),dpi=180)
 for e in edges:
  if e['type']=='line':ax.plot([e['a'][0],e['b'][0]],[e['a'][1],e['b'][1]],c='#536774',lw=1)
  else:
   start=0 if e['type']=='circle' else e['start'];end=360 if e['type']=='circle' else e['end']
   if end<=start:end+=360
   a=np.deg2rad(np.linspace(start,end,100));ax.plot(e['center'][0]+e['r']*np.cos(a),e['center'][1]+e['r']*np.sin(a),c='#536774',lw=1)
 for i,phase in enumerate(E['winding_sequence_ccw_from_tooth_1']):
  t=np.deg2rad(i*30);r=5.05
  ax.text(r*np.cos(t),r*np.sin(t),phase,ha='center',va='center',fontsize=11,color={'A':'#a66324','B':'#ad8424','C':'#954d2b'}[phase[0]],bbox=dict(fc='#fff5df',ec='none',pad=1))
  ax.text(8.25*np.cos(t),8.25*np.sin(t),str(i+1),ha='center',va='center',fontsize=11)
 for i in range(14):
  a=i*360/14;h=(360/14-.6)/2
  ax.add_patch(Wedge((0,0),7.65,a-h,a+h,width=.9,fc='#d96159' if i%2==0 else '#4c8bbd',ec='white',lw=.4))
 ax.text(0,0,'12 slots\n14 poles\nSTAR',ha='center',va='center',fontsize=12,color='#263b4b')
 ax.set_xlim(-9.5,9.5);ax.set_ylim(-9.5,9.5);ax.set_aspect('equal');ax.axis('off');fig.tight_layout();fig.savefig(IMG/'winding.png');plt.close(fig)
 return {'stator_dxf_errors':len(audit.errors),'stator_profile_edge_count':len(edges)}

PAGE=(1190.55,841.89);C=canvas.Canvas(str(PDFDIR/'HW17-R0-motor-wheel-design.pdf'),pagesize=PAGE)
C.setTitle('HW17-R0 | D17 x 9 motor wheel | Preliminary design');C.setAuthor('OpenWalkieTalkie engineering')
def clean(s):
 return str(s).replace('·','*').replace('²','^2').replace('³','^3').replace('√2','sqrt(2)').replace('–','-').replace('—','-').replace('•','-')
def text(x,y,s,size=12,color='#233d50'):
 C.setFillColor(HexColor(color));C.setFont('STSong-Light',size);C.drawString(x,y,clean(s))
def lines(x,y,ss,size=12,leading=21):
 for s in ss:text(x,y,s,size);y-=leading
 return y
def image(path,x,y,w,h):C.drawImage(str(path),x,y,width=w,height=h,preserveAspectRatio=True,anchor='c',mask='auto')
def page(title,n,subtitle):
 C.setFillColor(HexColor('#eef3f5'));C.rect(0,0,*PAGE,fill=1,stroke=0)
 C.setFillColor(HexColor('#173346'));C.rect(0,749,PAGE[0],93,fill=1,stroke=0)
 text(38,794,title,25,'#ffffff');text(38,765,subtitle,12,'#cbdbe5')
 C.setFillColor(HexColor('#d8a34c'));C.rect(38,715,1114,22,fill=1,stroke=0)
 text(48,721,'R0 初步设计 | 几何可评审；电磁、温升、寿命与最终工艺尚未验证。不得按此版直接量产。',11,'#33291a')
 text(38,22,'OpenWalkieTalkie / HW17-R0 / 2026-10-24 / 单位 mm，除非另注 / PDF 图示不按打印比例量取',10,'#617381')
 text(1094,22,f'{n} / 6',11,'#617381')
def table(x,y,width,headers,rows,colwidths,size=12,rowh=31):
 for ri,row in enumerate([headers]+rows):
  yy=y-ri*rowh;C.setFillColor(HexColor('#dce6eb' if ri==0 else ('#ffffff' if ri%2 else '#e9f0f3')));C.rect(x,yy-rowh,width,rowh,fill=1,stroke=0)
  xx=x
  for ci,s in enumerate(row):
   w=pdfmetrics.stringWidth(clean(s),'STSong-Light',size);fs=min(size,size*(colwidths[ci]-18)/max(w,1))
   text(xx+9,yy-rowh+10,str(s),fs);xx+=colwidths[ci]
 return y-(len(rows)+1)*rowh

def pdf():
 page('Φ17 × 9 mm 自研力反馈滚轮',1,'12 槽 / 14 极外转子 | 无按压功能 | 适配现有滚轮轴心位置')
 image(IMG/'cutaway.png',35,185,580,515)
 text(70,175,'剖切示意：绕组、轴承和芯片以包络实体表示。',12)
 table(640,687,510,['项目','R0 设计值'],[
 ('总外包络','Φ17.00 max × 9.00 max'),('模型实际轴向高度','8.80；预留 0.20 总尺寸余量'),('旋转触摸外圈','Φ17；轴向 7.60；POM 薄外圈'),('定子 / 磁环','Φ13 / Φ13.5 内径；径向气隙 0.25'),('有效叠厚','4.00；名义 20 片 × 0.20'),('轴承','2 × MR52ZZ，2 × 5 × 2.5'),('角度反馈','MT6701 QFN；独立两极测角磁铁'),('接口','三相 U/V/W + 测角供电及数据'),('安装状态','本体空间已检查；机壳连接支架待设计')],[150,360],12,36)
 lines(640,300,['用户已确认：','• 只旋转，与现有滚轮一致。','• 允许定制磁环和叠片铁芯。','• 保持滚轮外形 Φ17 × 9 mm。','本版范围：可编辑构型、分件 STEP、叠片 DXF、','绕组方案、解析估算和原始 CAD 干涉检查。'],13,26)
 C.showPage()
 page('总成剖面与关键尺寸',2,'局部坐标：Z 为电机轴；Z=0 对应机壳 Y=2.50；X=19.50、机壳 Z=39.00 为轴心')
 image(IMG/'section.png',30,310,1125,395)
 table(40,300,535,['径向结构（从中心向外）','尺寸'],[
 ('轴 / 轴承孔','Φ2.00 / Φ5.00，配合待轴承供应商确认'),('固定轴承座 / 定子内孔','Φ6.00 / Φ6.10；0.05 径向胶层'),('定子齿顶 / 磁钢内表面','Φ13.00 / Φ13.50；0.25 名义气隙'),('磁钢外径 / 钢轭内径','Φ15.30 / Φ15.40；0.05 径向胶层'),('钢轭外径 / 触摸外圈','Φ16.40 / Φ17.00；外圈单边 0.30')],[265,270],11,31)
 table(605,300,545,['轴向堆叠：局部 Z','区间'],[
 ('底座 / 测角小板','0–0.60 / 0–0.40'),('旋转外圈 / 转子端盖','1.20–8.80 / 8.00–8.80'),('磁钢 / 定子叠片','2.05–6.55 / 2.30–6.30'),('下轴承 / 上轴承','2.70–5.20 / 5.30–7.80'),('测角磁铁 / 最大芯片顶部','1.88–2.38 / 1.25；空气间隙 0.63')],[310,235],11,31)
 lines(40,90,['气隙公差预算建议：定子半径 0.025 + 磁环半径 0.025 + 装配偏心 0.05 + 轴系径向运动 0.02 + 热/胶层变化 0.03 = 0.15；余量 0.10。','以上为待分配的设计预算，不能代替制造公差和轴承寿命计算。所有转动部件不得与固定限位环接触；轴向游隙须用选配垫片控制。'],11,20)
 C.showPage()
 page('定子、磁环与绕组定义',3,'从局部 +Z 看向底座；1 号齿沿 +X，齿号逆时针增加；接线方向须用低电流定相测试确认')
 image(IMG/'winding.png',30,145,570,555)
 table(625,687,525,['参数','初版定义'],[
 ('槽极配合','12 槽 / 14 极；每相 4 个齿线圈串联'),('连接方式','星形；中性点不引出'),('每齿匝数 / 线径','32 匝 / 裸铜 Φ0.13，绝缘后 ≤ Φ0.145'),('排线包络','3 层：11 / 11 / 10；具体排线工艺待打样'),('绕组系数','解析计算 0.9330；三相轴相差 120°'),('磁环','14 片弧形磁钢拼环；相邻内表面 N/S 交替'),('磁钢材质','N35SH 建议值；牌号不代表气隙磁密'),('磁钢接缝 / 尺寸','每极间 0.6° / OD15.3 ID13.5 H4.5'),('叠片','0.20 电工钢；去毛刺、片间绝缘、边缘涂覆')],[155,370],11,34)
 lines(625,330,['齿号 / 绕组极性：','01 A+   02 A-   03 B-   04 B+   05 C+   06 C-','07 A-   08 A+   09 B+   10 B-   11 C-   12 C+','“+”定义：正电流从标记起线端流入时，齿顶成为 N 极。','“-”线圈反向绕制或交换起末端；全体遵循同一定义。','装配后检查三相线间电阻平衡、绝缘和反电势相位。','STATOR DXF：1:1 毫米，只有 CUT_PROFILE 层为轮廓。','切割补偿、齿顶圆角、材料 B-H 曲线和齿槽转矩未定版。'],12,25)
 C.showPage()
 page('电磁与热：可复算的初步估算',4,'所有磁密为假设情景；没有运行有限元分析。峰值段落力不等于可持续保持力。')
 base=A['scenarios'][1];p16=base['points'][1]
 table(40,687,1110,['量','解析结果 / 定义'],[
 ('相电阻 / 线间电阻，20°C',f"{A['phase_resistance_20C_ohm']:.2f} Ω / {A['line_line_resistance_20C_ohm']:.2f} Ω；含估计引线，必须以样机复测"),
 ('电流约定','Iq 为正弦相电流峰值；相电流 RMS = Iq / √2；不可直接当成电池输入电流'),
 ('公式','Kt = 1.5 p λ；λ = Nphase kw × (2 B1 r L / p)；Pcu = 1.5 Rphase Iq²'),
 ('模型参数','p=7；Nphase=128；kw=0.933；平均气隙半径 6.625 mm；有效叠厚 4.00 mm'),
 ('不包含','端部漏磁、饱和、铁损、驱动损耗、磁场谐波、胶层变化、轴承摩擦和真实散热路径')],[250,860],12,34)
 rows=[]
 for s in A['scenarios']:
  point=s['points'][1];rows.append([f"{s['assumed_fundamental_airgap_B_T']:.2f}",f"{s['Kt_mNm_per_A_peak']:.2f}",f"{point['Iq_peak_A']:.3f}",f"{point['copper_loss_20C_W']:.3f}",f"{point['copper_loss_80C_W']:.3f}"])
 text(40,460,'达到 1.6 mN·m 时，对气隙基波磁密 B1 的敏感性',16)
 table(40,440,1110,['假设 B1 / T','Kt / mN·m/A峰','Iq峰 / A','20°C 铜耗 / W','80°C 铜耗 / W'],rows,[200,240,200,235,235],13,42)
 rows=[]
 for p in base['points']:rows.append([f"{p['torque_mNm']:.1f}",f"{p['rim_force_N']:.3f}",f"{p['Iq_peak_A']:.3f}",f"{p['copper_loss_20C_W']:.3f}",f"{p['ideal_zero_speed_bus_min_20C_V']:.2f}"])
 text(40,249,'在假设 B1=0.45 T 下的三个力矩点（并非性能承诺）',16)
 table(40,228,1110,['力矩 / mN·m','轮缘切向力 / N','Iq峰 / A','铜耗 / W','理想最低母线 / V'],rows,[200,240,200,235,235],13,32)
 lines(40,75,['首版基准：复现现有约 1.6 mN·m 的段落力。0.5 W 铜耗仅作为探索预算；需测温后确定持续额定值、峰值持续时间和占空比。','最低母线值只含零速电阻压降及理想 SVPWM；实际还要加上电流变化、MOSFET、布线、电池压降和控制裕量。'],11,20)
 C.showPage()
 page('装配、安装与验证结果',5,'独立电机滚轮本体已建模；与机壳固定连接的转接支架和外部线束不在本版释放范围')
 image(IMG/'exploded.png',30,90,450,600)
 text(60,78,'爆炸示意：各组轴向平移展示，不代表装配运动路径。',10)
 lines(510,680,['建议装配顺序：','1. 叠片绝缘处理、绕线并检查三相电阻；将定子粘接到固定座。','2. 上轴承从上方入座；下轴承套在转轴上，从下方入座。','3. 内隔垫随轴装入；用外圈挡环定位轴承，检查自由转动。','4. 磁钢按 N/S 交替粘接到钢轭，测内圆跳动与磁极序列。','5. 转子套入定子；端盖与钢轭粘接，端盖轮毂固定到轴。','6. 选配轴向垫片，避免不可控预紧；安装测角磁铁及测角小板。','7. 装上外圈；检查全周气隙、无擦碰，再进行低电流定相。'],12,25)
 text(510,445,'原始 CAD 名义位置检查',16)
 rows=[[x['obstacle'].replace('RevB populated board in existing BodyV6.13 datum','Rev B 主板（沿用现有定位）'),f"{x['overlap_mm3']:.6f}",f"{x['minimum_distance_mm']:.3f}"] for x in V['fit_checks']]
 table(510,425,640,['对象','交叠体积 mm³','最小距离 mm'],rows,[330,165,145],11,34)
 lines(510,270,[f"本体内部实体交叠：{len(V['interferences'])} 项；STEP 回读有效：{V['step_roundtrip']['valid']}。",'本体包含：固定座、转子、轴系、磁钢、绕组包络及测角小板。','本体不包含：机壳转接支架、外部线束、主板驱动电路。','安装接口：底面 3×M1.4×0.3，PCD10.8，角度 30°/150°/270°。','原编码器板和四针线缆需要更换；本体不是现板直接替换件。','Rev B 的整板定位源自 V6.13，本次只检查滚轮区域；不代表','已经重新认证整板与 V8.3 外壳的全部装配关系。'],12,25)
 C.showPage()
 page('进入样机制造前的设计闭环',6,'本版已经完成几何构型；以下结果必须由仿真、制造评审或样机实测补齐')
 table(40,687,1110,['项目','下一步应获得的结果'],[
 ('电磁有限元','导入实际电工钢 B-H 和磁钢退磁曲线；计算 Kt、齿槽转矩、转矩波动、饱和及退磁裕量。'),
 ('测角磁路','验证自定 D4×0.5 两极磁铁；芯片面磁场需满足 20–100 mT，并检查轴承钢和电机通电干扰。'),
 ('热与电流','低电流起步；测定 1.6 mN·m 段落、持续顶住限位及闲置时的温升、电流、耗电与噪声。'),
 ('制造工艺','叠片边缘绝缘、32 匝排线可制造性、磁钢充磁夹具、胶层、动平衡与 0.25 气隙公差链。'),
 ('轴系与外壳','确认轴承配合、轴向游隙和防尘；完成转接支架，验证手指侧向载荷、跌落及长期磨损。'),
 ('驱动与主控','设计三相驱动、电流采样与硬件关断；单独测试 SF32 实时控制环，再叠加 UI、音频、USB/BLE。')],[170,940],12,46)
 text(40,338,'参考来源与事实边界',16)
 refs=[
 ('Alps EC05E1220401：现有段落力 1.6±1.3 mN·m','https://tech.alpsalpine.com/e/products/detail/EC05E1220401/'),
 ('EZO MR52ZZ：2×5×2.5 mm 尺寸与支承肩尺寸','https://www.ezo-brg.co.jp/english/product/spec.php?eid=00077'),
 ('MagnTek MT6701 Rev1.8：QFN 外形、20–100 mT 磁输入要求','https://www.magntek.com.cn/upload/pdf/202407/MT6701_Rev.1.8.pdf'),
 ('Arnold：N35SH 材料参数；不等于本电机的实际气隙磁密','https://www.arnoldmagnetics.com/products/neodymium-iron-boron-magnets/'),
 ('Maxon：电机常数、温升与换向方式的定义','https://support.maxongroup.com/hc/en-us/articles/360013761160-Motor-data-and-simulation')]
 y=310
 for title,url in refs:
  text(40,y,title,12);text(40,y-18,url,10,'#4f6b7d');C.linkURL(url,(40,y-21,1145,y+12),relative=0);y-=47
 text(40,72,'可编辑源：design.json、build.py、drawings.py。运行环境：Python 3.12、cadquery-ocp 8.x、numpy、matplotlib、ezdxf、reportlab。',11)
 text(40,50,'轴承、线圈和芯片模型为明确标注的包络；STEP 不是原厂细节模型。各页尺寸来自同一 R0 参数集，最终供应商工艺图仍需单独释放。',11)
 C.showPage();C.save()

if __name__=='__main__':
 render3d('assembled','assembled');render3d('cutaway','cutaway');render3d('exploded','exploded');section()
 checks=winding_and_dxf();pdf()
 (HERE/'drawing-verification.json').write_text(json.dumps(checks,indent=2)+'\n')
 print('Wrote PDF, illustrations and stator DXF',flush=True)
