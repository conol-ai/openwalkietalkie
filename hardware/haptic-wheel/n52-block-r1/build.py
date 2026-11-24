"""HW18-B1 bench motor RFQ. Exact OpenCascade solids, mm; no electromagnetic FEA.
Run from repo: target/hw17-r1-venv/bin/python hardware/haptic-wheel/n52-block-r1/build.py
"""
from pathlib import Path
import math, json, csv, hashlib
import numpy as np
import ezdxf
from OCP.BRep import BRep_Builder, BRep_Tool
from OCP.BRepAlgoAPI import BRepAlgoAPI_Common, BRepAlgoAPI_Cut, BRepAlgoAPI_Fuse
from OCP.BRepBndLib import BRepBndLib
from OCP.BRepBuilderAPI import BRepBuilderAPI_MakePolygon, BRepBuilderAPI_MakeFace, BRepBuilderAPI_Transform
from OCP.BRepCheck import BRepCheck_Analyzer
from OCP.BRepExtrema import BRepExtrema_DistShapeShape
from OCP.BRepGProp import BRepGProp
from OCP.BRepMesh import BRepMesh_IncrementalMesh
from OCP.BRepPrimAPI import BRepPrimAPI_MakeCylinder, BRepPrimAPI_MakeBox, BRepPrimAPI_MakePrism
from OCP.Bnd import Bnd_Box
from OCP.GProp import GProp_GProps
from OCP.gp import gp_Ax1, gp_Ax2, gp_Pnt, gp_Dir, gp_Trsf, gp_Vec
from OCP.IFSelect import IFSelect_RetDone
from OCP.STEPControl import STEPControl_Reader, STEPControl_Writer, STEPControl_AsIs
from OCP.TopAbs import TopAbs_SOLID, TopAbs_FACE, TopAbs_REVERSED
from OCP.TopExp import TopExp_Explorer
from OCP.TopLoc import TopLoc_Location
from OCP.TopoDS import TopoDS, TopoDS_Compound

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
CAD=HERE/'STEP'; DXF=HERE/'DXF'; QA=HERE/'verification'
for d in (CAD,DXF,QA): d.mkdir(parents=True,exist_ok=True)
REV='HW18-B1'; DATE='2026-10-30'
PARTS=[]; UNIQUE={}; PROFILES={}
PITCH=360/14; APOTHEM=8.0; MAG_IN=6.95
SEQ=['A+','A-','B-','B+','C+','C-','A-','A+','B+','B-','C-','C+']

def vol(s):
 p=GProp_GProps();BRepGProp.VolumeProperties_s(s,p);return p.Mass()
def bbox(s):
 b=Bnd_Box();BRepBndLib.AddOptimal_s(s,b,False,False)
 lo,hi=b.CornerMin(),b.CornerMax();return [lo.X(),lo.Y(),lo.Z(),hi.X(),hi.Y(),hi.Z()]
def solids(s):
 e=TopExp_Explorer(s,TopAbs_SOLID);out=[]
 while e.More():out.append(TopoDS.Solid(e.Current()));e.Next()
 return out
def compound(ss):
 b=BRep_Builder();s=TopoDS_Compound();b.MakeCompound(s)
 for sh in ss:b.Add(s,sh)
 return s
def cut(a,b):
 o=BRepAlgoAPI_Cut(a,b);o.Build();assert o.IsDone();return o.Shape()
def fuse(a,b):
 o=BRepAlgoAPI_Fuse(a,b);o.Build();assert o.IsDone();return o.Shape()
def common(a,b):return BRepAlgoAPI_Common(a,b).Shape()
def cyl(r,z,h,x=0,y=0):return BRepPrimAPI_MakeCylinder(gp_Ax2(gp_Pnt(x,y,z),gp_Dir(0,0,1)),r,h).Shape()
def ring(ro,ri,z,h):return cut(cyl(ro,z,h),cyl(ri,z-.01,h+.02))
def box(x,y,z,dx,dy,dz):return BRepPrimAPI_MakeBox(gp_Pnt(x,y,z),dx,dy,dz).Shape()
def move(s,x=0,y=0,z=0):
 t=gp_Trsf();t.SetTranslation(gp_Vec(x,y,z));return BRepBuilderAPI_Transform(s,t,True).Shape()
def rotate(s,angle):
 t=gp_Trsf();t.SetRotation(gp_Ax1(gp_Pnt(0,0,0),gp_Dir(0,0,1)),math.radians(angle));return BRepBuilderAPI_Transform(s,t,True).Shape()
def poly(points,z,h):
 pp=BRepBuilderAPI_MakePolygon()
 for x,y in points:pp.Add(gp_Pnt(x,y,z))
 pp.Close();return BRepPrimAPI_MakePrism(BRepBuilderAPI_MakeFace(pp.Wire()).Face(),gp_Vec(0,0,h)).Shape()
def polygon14(apothem=APOTHEM):
 r=apothem/math.cos(math.pi/14)
 return [(r*math.cos(math.radians((i+.5)*PITCH)),r*math.sin(math.radians((i+.5)*PITCH))) for i in range(14)]
def sector(ro,ri,a0,a1,z,h):
 pts=[(0,0)]+[(3*ro*math.cos(math.radians(a)),3*ro*math.sin(math.radians(a))) for a in (a0,a1)]
 return common(ring(ro,ri,z,h),poly(pts,z-.01,h+.02))
def write_step(path,sh):
 w=STEPControl_Writer();assert w.Transfer(sh,STEPControl_AsIs)==IFSelect_RetDone
 assert w.Write(str(path))==IFSelect_RetDone
def read_step(path):
 r=STEPControl_Reader();assert r.ReadFile(str(path))==IFSelect_RetDone;r.TransferRoots();return r.OneShape()
def distance(a,b):
 d=BRepExtrema_DistShapeShape(a,b);d.Perform();assert d.IsDone();return d.Value()
def register(pid,name,shape,material,process,qty,notes,z0=0,kind='make'):
 assert BRepCheck_Analyzer(shape).IsValid(),pid
 UNIQUE[pid]=dict(id=pid,name=name,shape=shape,material=material,process=process,qty=qty,notes=notes,z0=z0,kind=kind)
def instance(pid,sh=None,name=None,group='fixed',color='#8c9caa'):
 u=UNIQUE[pid];s=sh if sh is not None else u['shape']
 PARTS.append(dict(id=pid,name=name or pid,shape=s,group=group,color=color))

def build():
 # P01: simple POM sleeve, adhesive slip fit; no impossible narrow knurl tool.
 grip=ring(9,8.72,1.2,7.6)
 register('P01','POM 触摸外圈',grip,'黑色 POM-C','车削',1,'OD18.00±0.03; ID17.44 +0.03/0; H7.60±0.03; 胶接、无压配',1.2)
 instance('P01',group='rotor',color='#2e3740')
 # Polygon through bore plus round pilot counterbore for a manufacturable cap joint.
 yoke=cut(cyl(8.7,1.2,6.8),poly(polygon14(),1.19,6.82))
 yoke=cut(yoke,cyl(8.325,7.15,.86))
 register('P02','十四平面转子钢轭',yoke,'DT4C 电工纯铁；替代低碳钢须单列','车削 + 慢走丝 + 精整',1,'OD17.40 0/-0.01; 十四边形对边16.000±0.030; 上定位孔16.65 +0.015/0 深0.85±0.02',1.2)
 instance('P02',group='rotor',color='#81909b')
 # Cap: round pilot, relieved over stationary post, bonded to shaft after endplay setting.
 cap=ring(8.69,1.015,8.03,.77)
 cap=fuse(cap,ring(8.30,7.40,7.30,.73))
 cap=fuse(cap,ring(1.30,1.015,7.83,.20))
 cap=cut(cap,ring(3.20,1.40,8.02,.23))
 register('P03','转子端盖',cap,'6061-T6 铝','精密车削',1,'OD17.38±0.01; 定位外圆16.60±0.01; 轴胶接孔2.03 +0.01/0; 总高1.50±0.02',7.3)
 instance('P03',group='rotor',color='#b6c2cb')
 shaft=fuse(cyl(.9975,2.70,5.95),cyl(1.30,2.40,.30))
 register('P04','带肩转轴',shaft,'316L 奥氏体不锈钢，精加工后低磁性','车削 + 外圆磨削',1,'轴承段Φ2 -0.003/-0.006; 肩Φ2.60±0.01; 长6.25±0.02; 肩根R≤0.05',2.4)
 instance('P04',group='rotor',color='#c6d0d7')
 # User's actual bought magnet is a purchased item, not a custom part.
 mag=box(MAG_IN,-1.5,2.30,1,3,4)
 register('C01','用户自备 N52 电机磁铁',mag,'N52 烧结钕铁硼，保留原镀层','采购件 / 来料检验',14,'4轴向×3切向×1径向，沿1mm充磁；本方案接收三向±0.05；超差重算',2.3,'buy')
 for i in range(14):instance('C01',rotate(mag,i*PITCH),f'C01_{i+1:02d}_{"N" if i%2==0 else "S"}','magnet','#d26058' if i%2==0 else '#4287b8')
 # Carrier: long through bearing seat with a lower shoulder; eliminate original 0.1-mm internal web.
 base=cyl(7,0,.6)
 base=cut(base,cyl(2.8,-.01,.41))
 carrier=fuse(base,cyl(3,.6,7.45))
 carrier=cut(carrier,cyl(2.50,.39,2.12))
 carrier=cut(carrier,cyl(2.25,2.49,.22))
 carrier=cut(carrier,cyl(2.5025,2.7,5.36))
 for a in (30,150,270):
  x,y=5.4*math.cos(math.radians(a)),5.4*math.sin(math.radians(a))
  carrier=fuse(carrier,cyl(1.2,.6,1.0,x,y))
  # Simplified M1.4 minor bore; the drawing is the thread authority.
  carrier=cut(carrier,cyl(.55,-.01,1.41,x,y))
 carrier=cut(carrier,rotate(box(0,-1,-.01,7.1,2,.41),120))
 carrier=cut(carrier,cyl(.45,-.01,.62,0,4.7))
 register('P05','定子及轴承固定座',carrier,'6061-T6 铝，无阳极氧化','车铣复合 / 同基准精镗',1,'两轴承共用Φ5 +0.008/+0.003通长座孔; 定子定位Φ6 0/-0.01; 3×M1.4×0.3-6H盲孔',0)
 instance('P05',color='#aebdc5')
 # Exact R0 lamination shape: ring, radial webs, circular pole shoes.
 core=ring(4,3.05,2.3,4)
 for i in range(12):
  tooth=fuse(box(3.9,-.4,2.3,2.25,.8,4),sector(6.5,6.1,-11,11,2.3,4))
  core=fuse(core,rotate(tooth,i*30))
 lam=common(move(core,z=-2.3),cyl(10,0,.2))
 register('P06','定子冲片 / 切割片',lam,'0.20mm 无取向电工钢，双面绝缘','精密激光或蚀刻；慢走丝须重做边缘绝缘',20,'OD13.00±0.02; ID6.10 +0.02/0; 轮廓±0.02; 毛刺≤0.010; 叠厚优先',0)
 register('A02','绕线定子组件',core,'P06叠片 + 漆包铜线','叠片粘接 + 绕线 + 检验',1,'成叠4.00±0.03含绝缘；每齿32匝，12齿，3相星形；不是实心铁件',2.3,'assembly')
 instance('A02')
 for i,phase in enumerate(SEQ):
  coil=box(4.2,-.98,1.72,1.75,1.96,5.16)
  coil=cut(coil,box(4.19,-.5,2.2,1.77,1,4.2))
  PARTS.append(dict(id='E01',name=f'E01_coil_{i+1:02d}_{phase}',shape=rotate(coil,i*30),group='coil',color={'A':'#c58c4a','B':'#d4b767','C':'#ad704c'}[phase[0]]))
 bearing=ring(2.5,1,2.7,2.5)
 register('C02','MR52ZZ 轴承',bearing,'EZO MR52ZZ 或经确认的同规格低摩擦件','采购，按实物选配轴与孔',2,'2×5×2.5；挡肩da=2.6、Da=4.5；倒角r最小0.1；供应商报游隙和摩擦矩',2.7,'buy')
 instance('C02',name='C02_lower',group='bearing',color='#cbd3da')
 instance('C02',move(bearing,z=2.6),name='C02_upper',group='bearing',color='#cbd3da')
 ins=ring(1.3,1.025,5.2,.1)
 register('P07','内圈选配隔垫',ins,'316L 精密薄片','光化学蚀刻 / 精密垫片',1,'OD2.60±0.01; ID2.05 +0.02/0; 名义0.10，备0.08/0.09/0.11/0.12；厚度±0.003',5.2)
 instance('P07',group='rotor')
 ous=ring(2.49,2.25,5.2,.1)
 register('P08','外圈隔垫',ous,'316L 精密薄片','光化学蚀刻 / 精密垫片',1,'OD4.98 0/-0.02; ID4.50 +0.02/0; t0.100±0.003; 平面度0.005',5.2)
 instance('P08')
 ret=ring(2.49,2.25,7.82,.20)
 register('P09','上轴承外圈挡环',ret,'316L 精密薄片','光化学蚀刻 / 精密垫片',1,'OD4.98 0/-0.02; ID4.50 +0.02/0; t0.20±0.01; 胶接位置装配调节',7.82)
 instance('P09')
 smag=cyl(2,1.88,.5)
 register('C03','两极测角磁铁',smag,'N35SH 烧结钕铁硼，Ni-Cu-Ni','优先现货；无现货单列定制报价',1,'Φ4.00±0.03 ×0.50±0.03；直径方向两极充磁；不可用厚度充磁C01替代',1.88,'buy')
 instance('C03',group='rotor',color='#ac89af')
 pcb=fuse(cyl(2.75,0,.4),rotate(box(0,-.8,0,6.8,1.6,.15),120))
 for pid,shape,color in [('E02_sensor_flex',pcb,'#438976'),('E03_MT6701',rotate(box(-1.55,-1.55,.45,3.1,3.1,.8),-15),'#263e49'),('E04_capacitor',rotate(box(1.65,-.15,.45,.6,.3,.3),-15),'#c1bda6')]:
  PARTS.append(dict(id=pid,name=pid,shape=shape,group='sensor',color=color))
 # Cup fixture centers the yoke OD and supports magnet lower edges. Per-face PET
 # shims compensate bought t=.95..1.05: shim s=1.05-t, nominal s=.05.
 mandrel=cyl(11,0,2)
 mandrel=fuse(mandrel,ring(11,8.705,2,1))
 mandrel=fuse(mandrel,poly(polygon14(7.8),2,1.1))
 mandrel=fuse(mandrel,poly(polygon14(6.9),3.1,4))
 mandrel=cut(mandrel,cyl(3,-.01,7.12))
 register('J01','磁铁粘接定位工装',mandrel,'POM-C，自然色','车削 + 铣削十四平面',1,'定位芯对边13.800±0.010; 外圆定位孔17.41 +0.01/0; PET垫厚=1.05-实测磁铁厚度；名义0.05',0,'fixture')
 plate=box(-20,-15,-4,40,30,4)
 plate=cut(plate,cyl(4,-4.01,4.02))
 plate=cut(plate,rotate(box(0,-1,-.45,26,2,.46),120))
 for a in (30,150,270):
  x,y=5.4*math.cos(math.radians(a)),5.4*math.sin(math.radians(a))
  plate=cut(plate,cyl(.8,-4.01,4.02,x,y))
 for x in (-16,16):
  for y in (-11,11):plate=cut(plate,cyl(1.7,-4.01,4.02,x,y))
 register('J02','台架固定底板',plate,'6061-T6 铝','铣削',1,'40×30×4；3×Φ1.60 PCD10.8；4×Φ3.4；中心Φ8；传感器出线槽',-4,'fixture')

def mesh(s):
 BRepMesh_IncrementalMesh(s,.04,False,.15,True).Perform();vv=[];tt=[]
 e=TopExp_Explorer(s,TopAbs_FACE)
 while e.More():
  f=TopoDS.Face(e.Current());loc=TopLoc_Location();t=BRep_Tool.Triangulation_s(f,loc)
  if t is not None:
   tr=loc.Transformation();off=len(vv)
   for i in range(1,t.NbNodes()+1):
    p=t.Node(i).Transformed(tr);vv.append([p.X(),p.Y(),p.Z()])
   for i in range(1,t.NbTriangles()+1):
    a,b,c=t.Triangle(i).Get()
    if f.Orientation()==TopAbs_REVERSED:b,c=c,b
    tt.append([off+a-1,off+b-1,off+c-1])
  e.Next()
 return {'vertices':vv,'triangles':tt}

def colored_assembly(path):
 from OCP.XCAFApp import XCAFApp_Application
 from OCP.XCAFDoc import XCAFDoc_DocumentTool,XCAFDoc_ColorGen
 from OCP.TDocStd import TDocStd_Document
 from OCP.TCollection import TCollection_ExtendedString
 from OCP.TDataStd import TDataStd_Name
 from OCP.Quantity import Quantity_Color,Quantity_TOC_RGB
 from OCP.STEPCAFControl import STEPCAFControl_Writer
 app=XCAFApp_Application.GetApplication_s();doc=TDocStd_Document(TCollection_ExtendedString(REV));app.NewDocument(TCollection_ExtendedString('MDTV-XCAF'),doc)
 st=XCAFDoc_DocumentTool.ShapeTool_s(doc.Main());ct=XCAFDoc_DocumentTool.ColorTool_s(doc.Main());root=st.NewShape();TDataStd_Name.Set_s(root,TCollection_ExtendedString(REV+' bench prototype'))
 for p in PARTS:
  lab=st.AddShape(p['shape'],False);TDataStd_Name.Set_s(lab,TCollection_ExtendedString(p['name']))
  h=p['color'].lstrip('#');rgb=[int(h[i:i+2],16)/255 for i in (0,2,4)]
  ct.SetColor(lab,Quantity_Color(*rgb,Quantity_TOC_RGB),XCAFDoc_ColorGen);st.AddComponent(root,lab,TopLoc_Location())
 st.UpdateAssemblies();w=STEPCAFControl_Writer();w.SetColorMode(True);w.SetNameMode(True);assert w.Transfer(doc,STEPControl_AsIs);assert w.Write(str(path))==IFSelect_RetDone

def lamination_dxf():
 # Boundary exactly matches R0 B-rep union. Lines and circular arcs, not polylines.
 doc=ezdxf.new('R2010');doc.units=4;m=doc.modelspace();doc.layers.new('CUT_PROFILE',dxfattribs={'color':7});a={'layer':'CUT_PROFILE'}
 def point(r,deg):return (r*math.cos(math.radians(deg)),r*math.sin(math.radians(deg)))
 alpha=math.degrees(math.asin(.4/6.1));beta=math.degrees(math.asin(.4/4))
 m.add_circle((0,0),3.05,dxfattribs=a)
 for i in range(12):
  q=i*30
  m.add_arc((0,0),6.5,q-11,q+11,dxfattribs=a)
  m.add_line(point(6.5,q+11),point(6.1,q+11),dxfattribs=a)
  m.add_arc((0,0),6.1,q+alpha,q+11,dxfattribs=a)
  m.add_line(point(6.1,q+alpha),point(4,q+beta),dxfattribs=a)
  m.add_arc((0,0),4,q+beta,q+30-beta,dxfattribs=a)
  m.add_line(point(4,q+30-beta),point(6.1,q+30-alpha),dxfattribs=a)
  m.add_arc((0,0),6.1,q+30-11,q+30-alpha,dxfattribs=a)
  m.add_line(point(6.1,q+30-11),point(6.5,q+30-11),dxfattribs=a)
 doc.saveas(DXF/f'{REV}-P06-cut-profile-1to1.dxf')
 # Polygon bore and sheet ring profiles: exact 1:1 tooling geometry only.
 for pid,rs in [('P07',(1.3,1.025)),('P08',(2.49,2.25)),('P09',(2.49,2.25))]:
  d=ezdxf.new('R2010');d.units=4;m=d.modelspace()
  for r in rs:m.add_circle((0,0),r)
  d.saveas(DXF/f'{REV}-{pid}-cut-profile-1to1.dxf')
 for pid,ap in [('P02',8.0),('J01',6.90)]:
  d=ezdxf.new('R2010');d.units=4;m=d.modelspace();m.add_lwpolyline(polygon14(ap),close=True)
  if pid=='P02':m.add_circle((0,0),8.7)
  else:
   m.add_circle((0,0),3)
   m.add_circle((0,0),11)
   m.add_circle((0,0),8.705)
   m.add_lwpolyline(polygon14(7.8),close=True)
  d.saveas(DXF/f'{REV}-{pid}-profile-1to1.dxf')

def validate():
 report={'revision':REV,'scope':'Nominal CAD + explicit tolerance budgets; no FEA or rated performance; not an original-case fit release','interferences':[],'part_checks':[],'step_roundtrip':[]}
 for pid,u in UNIQUE.items():
  s=u['shape'];path=CAD/f'{REV}-{pid}.step';write_step(path,move(s,z=-u['z0']))
  reopened=read_step(path);err=abs(vol(s)-vol(reopened))
  row={'id':pid,'valid':BRepCheck_Analyzer(reopened).IsValid(),'solids':len(solids(reopened)),'volume_mm3':vol(s),'volume_error_mm3':err,'bbox_mm':bbox(reopened)}
  report['step_roundtrip'].append(row);assert row['valid'] and row['solids']==1 and err<1e-6,row
 for i,a in enumerate(PARTS):
  ba=bbox(a['shape']);report['part_checks'].append({'name':a['name'],'valid':BRepCheck_Analyzer(a['shape']).IsValid(),'bbox_mm':ba})
  for b in PARTS[i+1:]:
   bb=bbox(b['shape'])
   if any(min(ba[k+3],bb[k+3])-max(ba[k],bb[k])<1e-7 for k in range(3)):continue
   v=vol(common(a['shape'],b['shape']))
   if v>1e-6:report['interferences'].append({'a':a['name'],'b':b['name'],'volume_mm3':v})
 allshape=compound([p['shape'] for p in PARTS]);report['envelope_bbox_mm']=bbox(allshape)
 report['outside_D18_H8p8_mm3']=vol(cut(allshape,cyl(9,-.001,8.802)))
 colored_assembly(CAD/f'{REV}-A01-assembly.step')
 rt=read_step(CAD/f'{REV}-A01-assembly.step');report['assembly_roundtrip']={'valid':BRepCheck_Analyzer(rt).IsValid(),'solids':len(solids(rt)),'volume_error_mm3':abs(vol(rt)-vol(allshape))}
 moving=[p['shape'] for p in PARTS if p['group'] in ('rotor','magnet')]
 stationary=[p['shape'] for p in PARTS if p['group'] in ('fixed','coil','sensor')]
 write_step(CAD/f'{REV}-A03-rotor.step',compound(moving))
 write_step(CAD/f'{REV}-A04-fixed-assembly.step',compound(stationary))
 wound=compound([p['shape'] for p in PARTS if p['group']=='coil' or p['id']=='A02'])
 wound_path=CAD/f'{REV}-A02-winding-envelope.step';write_step(wound_path,wound)
 wound_rt=read_step(wound_path)
 report['wound_stator_reference_roundtrip']={'valid':BRepCheck_Analyzer(wound_rt).IsValid(),'solids':len(solids(wound_rt)),'volume_error_mm3':abs(vol(wound_rt)-vol(wound))}
 assert report['wound_stator_reference_roundtrip']['valid'] and report['wound_stator_reference_roundtrip']['volume_error_mm3']<1e-5
 write_step(CAD/f'{REV}-A05-bench-setup.step',compound([allshape,UNIQUE['J02']['shape']]))
 # Angle samples supplement the conservative continuous 360-degree envelopes below.
 rot=compound(moving);fixed=compound(stationary)
 report['rotor_stationary_angle_samples']=[]
 for angle in (0,5,15,45,90,180,270,355):
  sh=rotate(rot,angle);v=vol(common(sh,fixed))
  report['rotor_stationary_angle_samples'].append({'angle_deg':angle,'overlap_mm3':v})
  assert v<1e-6,(angle,v)
 # Continuous conservative swept envelopes: circles for all rotating parts.
 swept=[ring(9,8.72,1.2,7.6),ring(8.7,8.0,1.2,6.8),ring(math.hypot(7.95,1.5),6.95,2.3,4),UNIQUE['P03']['shape'],UNIQUE['P04']['shape'],UNIQUE['P07']['shape'],UNIQUE['C03']['shape']]
 sweptshape=compound(swept);sv=vol(common(sweptshape,fixed));assert sv<1e-6,sv
 report['continuous_rotational_swept_overlap_mm3']=sv
 report['swept_rotor_to_stationary_minimum_mm']=distance(sweptshape,fixed)
 h=math.pi/14
 gap=2*MAG_IN*math.sin(h)-3*math.cos(h)
 # Incoming t±.05; flat radial datum ±.015; bond .03..07. Relative fixture registration .03.
 inner_min=8-.015-.07-1.05
 worst_pair=2*inner_min*math.sin(h)-3.05*math.cos(h)-.03-.015
 radial_budget=.015+.02+.05+.015+.03+.02+.02
 report['geometry']={'magnet_inner_face_radius_mm':MAG_IN,'nominal_center_airgap_mm':MAG_IN-6.5,'nominal_edge_radial_airgap_mm':math.hypot(MAG_IN,1.5)-6.5,'nominal_adjacent_magnet_clearance_mm':gap,'worst_case_adjacent_magnet_clearance_budget_mm':worst_pair,'nominal_yoke_min_wall_mm':8.7-8/math.cos(h),'nominal_upper_counterbore_wall_mm':8.7-8.325,'nominal_sensor_airgap_mm':1.88-1.25,'rotor_nominal_OD_mm':18,'height_mm':8.8}
 report['tolerance_budget']={'airgap_nominal_mm':.45,'flat_radial_error_mm':.015,'bond_error_mm':.02,'magnet_thickness_error_mm':.05,'stator_radius_error_mm':.015,'assembly_eccentricity_radial_mm':.03,'bearing_radial_motion_budget_mm':.02,'thermal_adhesive_allowance_mm':.02,'sum_mm':radial_budget,'remaining_airgap_mm':.45-radial_budget,'assembly_acceptance_min_airgap_mm':.25,'note':'Budget is a requirement, not a measured capability. 0.25 mm acceptance still required under gentle side load; dimensions at 20±2 C.'}
 # Fixture clearances and overlap (plate is deliberately tangent to carrier base).
 report['bench_plate_overlap_mm3']=vol(common(allshape,UNIQUE['J02']['shape']))
 assert report['bench_plate_overlap_mm3']<1e-6
 jig=UNIQUE['J01']['shape']
 jig_load=compound([move(UNIQUE['P02']['shape'],z=.8)]+[move(p['shape'],z=.8) for p in PARTS if p['group']=='magnet'])
 report['magnet_fixture_overlap_mm3']=vol(common(jig,jig_load))
 assert report['magnet_fixture_overlap_mm3']<1e-6
 write_step(CAD/f'{REV}-J01-loaded-reference.step',compound([jig,jig_load]))
 assert not report['interferences'],report['interferences']
 assert report['outside_D18_H8p8_mm3']<1e-6
 assert report['assembly_roundtrip']['valid'] and report['assembly_roundtrip']['volume_error_mm3']<1e-5
 assert worst_pair>.03
 (QA/'geometry-check.json').write_text(json.dumps(report,indent=2))
 return report

if __name__=='__main__':
 build();print('Built solids; checking STEP exports and all part pairs...',flush=True)
 lamination_dxf();report=validate()
 catalog={pid:{k:v for k,v in u.items() if k!='shape'} for pid,u in UNIQUE.items()}
 (HERE/'catalog.json').write_text(json.dumps(catalog,ensure_ascii=False,indent=2))
 meshes=[]
 for p in PARTS:meshes.append({k:v for k,v in p.items() if k!='shape'}|mesh(p['shape']))
 (QA/'mesh.json').write_text(json.dumps(meshes,separators=(',',':')))
 specs={'revision':REV,'date':DATE,'purpose':'Bench prototype supplier quotation only','user_authorized_envelope_relaxation':True,'nominal_envelope_mm':[18,8.8],'inspection_envelope_max_mm':[18.05,9.0],'magnet_size_axial_tangential_radial_mm':[4,3,1],'magnet_incoming_dimension_tolerance_mm':.05,'magnet_grade':'N52','pole_count':14,'slot_count':12,'stator_OD_mm':13,'stack_mm':4,'turns_per_tooth':32,'wire_Cu_diameter_mm':.13,'max_finished_wire_OD_mm':.145,'phase_sequence_CCW':SEQ,'changed_from_R0':['N52 rectangular magnets,14-flat yoke','Nominal OD18, central gap0.45','Round pilot counterbore for cap','Carrier lower shoulder + separate outer spacer replaces thin central web','Defined slip-fit selection and measured axial endplay','Plain POM sleeve and bench fixtures'],'limitations':['No validated torque, thermal rating, demagnetization margin or encoder field','Original enclosure fit not retained or approved','Bearing and winding geometry are envelopes; thread modeled at nominal minor diameter','Generic edge breaks governed by 2D notes, not modeled']}
 (HERE/'design.json').write_text(json.dumps(specs,ensure_ascii=False,indent=2))
 with (HERE/'BOM_询价清单.csv').open('w',encoding='utf-8-sig',newline='') as f:
  w=csv.writer(f);w.writerow(['图号','名称','类型','单台数量','材料','工艺','备注'])
  for u in UNIQUE.values():w.writerow([REV+'-'+u['id'],u['name'],u['kind'],u['qty'],u['material'],u['process'],u['notes']])
 print(json.dumps(report['geometry'],indent=2),flush=True)
