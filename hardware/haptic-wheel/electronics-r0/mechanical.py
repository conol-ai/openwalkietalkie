"""Encoder clearance envelope, in the motor's CAD coordinates (mm)."""
from pathlib import Path
import math,json,hashlib
from OCP.BRep import BRep_Builder
from OCP.BRepAlgoAPI import BRepAlgoAPI_Common
from OCP.BRepBuilderAPI import BRepBuilderAPI_MakePolygon,BRepBuilderAPI_MakeFace,BRepBuilderAPI_Transform
from OCP.BRepCheck import BRepCheck_Analyzer
from OCP.BRepGProp import BRepGProp
from OCP.BRepPrimAPI import BRepPrimAPI_MakePrism,BRepPrimAPI_MakeCylinder,BRepPrimAPI_MakeBox
from OCP.GProp import GProp_GProps
from OCP.gp import gp_Pnt,gp_Vec,gp_Ax1,gp_Dir,gp_Trsf
from OCP.STEPControl import STEPControl_Reader,STEPControl_Writer,STEPControl_AsIs
from OCP.IFSelect import IFSelect_RetDone
from OCP.TopoDS import TopoDS_Compound

ROOT=Path(__file__).resolve().parent;OUT=ROOT/'mechanical'
def volume(s):
 p=GProp_GProps();BRepGProp.VolumeProperties_s(s,p);return p.Mass()
def assembly(parts):
 b=BRep_Builder();a=TopoDS_Compound();b.MakeCompound(a)
 for s in parts:b.Add(a,s)
 return a
def box(x,y,z,a,b,c):return BRepPrimAPI_MakeBox(gp_Pnt(x,y,z),a,b,c).Shape()
def export(path,shape):
 assert BRepCheck_Analyzer(shape).IsValid()
 w=STEPControl_Writer();assert w.Transfer(shape,STEPControl_AsIs)==IFSelect_RetDone
 assert w.Write(str(path))==IFSelect_RetDone
def read(path):
 r=STEPControl_Reader();assert r.ReadFile(str(path))==IFSelect_RetDone;r.TransferRoots();return r.OneShape()
def rotate(s,deg):
 t=gp_Trsf();t.SetRotation(gp_Ax1(gp_Pnt(0,0,0),gp_Dir(0,0,1)),math.radians(deg));return BRepBuilderAPI_Transform(s,t,True).Shape()

# Recover the exact fabrication outline instead of approximating a second outline.
edges=json.loads((OUT/'encoder-outline.json').read_text());a,b=edges.pop(0);points=[a,b]
while edges:
 for i,(a,b) in enumerate(edges):
  if math.dist(points[-1],a)<1e-5:points.append(b);edges.pop(i);break
  if math.dist(points[-1],b)<1e-5:points.append(a);edges.pop(i);break
 else:raise RuntimeError('Open outline')
assert math.dist(points[0],points[-1])<1e-5
poly=BRepBuilderAPI_MakePolygon()
for x,y in points[:-1]:poly.Add(gp_Pnt(x,y,.25))
poly.Close();flex=BRepPrimAPI_MakePrism(BRepBuilderAPI_MakeFace(poly.Wire()).Face(),gp_Vec(0,0,.15)).Shape()
stiff=BRepPrimAPI_MakeCylinder(2.75,.25).Shape()
chip=box(-1.55,-1.55,.45,3.1,3.1,.8)
cap=box(-.325,2.075,.45,.65,.35,.35)
parts=[flex,stiff,chip,cap];model=assembly(parts)
export(OUT/'HW17-encoder-r0-envelope.step',model)
export(OUT/'HW17-encoder-r0-flex.step',flex)
qa={'coordinate_system':'Shaft center X=Y=0; carrier underside Z=0; tail +135 degrees',
 'flex_z_mm':[.25,.40],'island_total_thickness_mm':.4,'chip_max_top_z_mm':1.25,
 'nominal_magnet_bottom_z_mm':1.88,'nominal_magnet_gap_mm':.63,
 'models':'Clearance envelopes, not supplier package detail or copper solids',
 'valid_solids':all(BRepCheck_Analyzer(s).IsValid() for s in parts),'fit_checks':[]}
for path,angle in [(ROOT.parent/'d17x9-r0/cad/06_stator_carrier.step',0),(ROOT.parent/'n52-block-r1/STEP/HW18-B1-P05.step',-15)]:
 if not path.exists():continue
 carrier=read(path);enc=rotate(model,angle)
 overlap=volume(BRepAlgoAPI_Common(carrier,enc).Shape())
 qa['fit_checks'].append({'carrier':str(path.relative_to(ROOT.parent)),
 'carrier_sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'encoder_rotation_deg':angle,'intersection_volume_mm3':overlap})
 assert overlap<1e-6,(path,overlap)
 if angle:export(OUT/'HW17-encoder-r0-envelope-120deg.step',enc)
qa['step_roundtrip_volume_error_mm3']=abs(volume(read(OUT/'HW17-encoder-r0-envelope.step'))-sum(volume(s) for s in parts))
assert qa['step_roundtrip_volume_error_mm3']<1e-5
(ROOT/'review/mechanical-check.json').write_text(json.dumps(qa,indent=2))
print(json.dumps(qa,indent=2))
