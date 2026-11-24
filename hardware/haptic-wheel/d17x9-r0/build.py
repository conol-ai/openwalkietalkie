"""Rebuild HW17-R0 exact B-rep CAD, mechanical checks and analytical estimates.

Run from repository root with target/cnc-venv/bin/python.
Units: mm. Local Z is the rotation axis; local +X points to case +X.
The model is a preliminary design. Coils/bearings/ICs are explicit envelopes.
"""
import csv, hashlib, json, math, os
from pathlib import Path
import numpy as np
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
from OCP.gp import gp_Ax1, gp_Pnt, gp_Dir, gp_Trsf, gp_Vec
from OCP.IFSelect import IFSelect_RetDone
from OCP.STEPControl import STEPControl_Reader, STEPControl_Writer, STEPControl_AsIs
from OCP.TopAbs import TopAbs_SOLID, TopAbs_FACE, TopAbs_REVERSED
from OCP.TopExp import TopExp_Explorer
from OCP.TopLoc import TopLoc_Location
from OCP.TopoDS import TopoDS, TopoDS_Compound

ROOT=Path(__file__).resolve().parents[3]
HERE=Path(__file__).resolve().parent
OUT=HERE/'cad';OUT.mkdir(exist_ok=True)
P=json.loads((HERE/'design.json').read_text());G=P['geometry'];E=P['electromagnetics']
PARTS=[]

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
def cyl(r,z,h,x=0,y=0):
 return BRepPrimAPI_MakeCylinder(gp_Ax2(gp_Pnt(x,y,z),gp_Dir(0,0,1)),r,h).Shape()
from OCP.gp import gp_Ax2
def ring(ro,ri,z,h):return cut(cyl(ro,z,h),cyl(ri,z-.01,h+.02))
def box(x,y,z,dx,dy,dz):return BRepPrimAPI_MakeBox(gp_Pnt(x,y,z),dx,dy,dz).Shape()
def translate(s,x=0,y=0,z=0):
 t=gp_Trsf();t.SetTranslation(gp_Vec(x,y,z));return BRepBuilderAPI_Transform(s,t,True).Shape()
def rotate(s,ang):
 t=gp_Trsf();t.SetRotation(gp_Ax1(gp_Pnt(0,0,0),gp_Dir(0,0,1)),math.radians(ang));return BRepBuilderAPI_Transform(s,t,True).Shape()
def sector(ro,ri,ang0,ang1,z,h):
 # Exact cylindrical faces, cut by a wedge; no polygonized magnet/airgap arcs.
 pp=BRepBuilderAPI_MakePolygon();pp.Add(gp_Pnt(0,0,z-.01))
 for a in (ang0,ang1):pp.Add(gp_Pnt(3*ro*math.cos(math.radians(a)),3*ro*math.sin(math.radians(a)),z-.01))
 pp.Close();w=BRepPrimAPI_MakePrism(BRepBuilderAPI_MakeFace(pp.Wire()).Face(),gp_Vec(0,0,h+.02)).Shape()
 return BRepAlgoAPI_Common(ring(ro,ri,z,h),w).Shape()
def write_step(path,sh):
 w=STEPControl_Writer();assert w.Transfer(sh,STEPControl_AsIs)==IFSelect_RetDone
 assert w.Write(str(path))==IFSelect_RetDone
def write_colored_assembly(path):
 from OCP.XCAFApp import XCAFApp_Application
 from OCP.XCAFDoc import XCAFDoc_DocumentTool,XCAFDoc_ColorGen
 from OCP.TDocStd import TDocStd_Document
 from OCP.TCollection import TCollection_ExtendedString
 from OCP.TDataStd import TDataStd_Name
 from OCP.Quantity import Quantity_Color,Quantity_TOC_RGB
 from OCP.STEPCAFControl import STEPCAFControl_Writer
 app=XCAFApp_Application.GetApplication_s();doc=TDocStd_Document(TCollection_ExtendedString('HW17'))
 app.NewDocument(TCollection_ExtendedString('MDTV-XCAF'),doc)
 st=XCAFDoc_DocumentTool.ShapeTool_s(doc.Main());ct=XCAFDoc_DocumentTool.ColorTool_s(doc.Main())
 root=st.NewShape();TDataStd_Name.Set_s(root,TCollection_ExtendedString('HW17-R0 preliminary motor wheel'))
 for p in PARTS:
  label=st.AddShape(p['shape'],False);TDataStd_Name.Set_s(label,TCollection_ExtendedString(p['name']))
  h=p['color'].lstrip('#');rgb=[int(h[i:i+2],16)/255 for i in (0,2,4)]
  ct.SetColor(label,Quantity_Color(*rgb,Quantity_TOC_RGB),XCAFDoc_ColorGen)
  st.AddComponent(root,label,TopLoc_Location())
 st.UpdateAssemblies()
 writer=STEPCAFControl_Writer();writer.SetColorMode(True);writer.SetNameMode(True)
 assert writer.Transfer(doc,STEPControl_AsIs)
 assert writer.Write(str(path))==IFSelect_RetDone
def read_step(path):
 r=STEPControl_Reader();assert r.ReadFile(str(path))==IFSelect_RetDone;r.TransferRoots();return r.OneShape()
def add(name,sh,group,color,material,note=''):
 assert BRepCheck_Analyzer(sh).IsValid(),name
 assert vol(sh)>0,name
 PARTS.append(dict(name=name,shape=sh,group=group,color=color,material=material,note=note))

def build():
 PARTS.clear()
 rz,zt=G['rotor_bottom_z_mm'],G['top_z_mm'];yt=G['rotor_yoke_top_z_mm']
 rg,ry=G['grip_od_mm']/2,G['grip_id_mm']/2;yi=G['rotor_yoke_id_mm']/2
 sz,sl=G['stator_bottom_z_mm'],G['stator_stack_mm'];so,si=G['stator_od_mm']/2,G['stator_id_mm']/2
 sy=G['stator_yoke_od_mm']/2;tr=G['tooth_tip_root_radius_mm'];tw=G['tooth_width_mm'];ta=G['tooth_tip_half_angle_deg']
 bo,bi,bw=G['bearing_od_mm']/2,G['bearing_id_mm']/2,G['bearing_width_mm'];bl,bu=G['bearing_lower_z_mm'],G['bearing_upper_z_mm']
 bz=G['carrier_base_thickness_mm'];br=G['carrier_base_od_mm']/2;pr=G['carrier_post_od_mm']/2
 ab=G['bearing_inner_abutment_od_mm']/2;stop=G['bearing_outer_stop_id_mm']/2
 # Rotating outer shell: POM grip is supported continuously by the steel yoke.
 grip=ring(G['grip_od_mm']/2,G['grip_id_mm']/2,G['rotor_bottom_z_mm'],G['top_z_mm']-G['rotor_bottom_z_mm'])
 for i in range(36):
  a=math.radians(i*10);grip=cut(grip,cyl(.18,1.35,7.30,8.56*math.cos(a),8.56*math.sin(a)))
 add('01_grip_ring',grip,'rotor','#313c48','POM','36 shallow axial flutes, max OD17; black matte finish')
 yoke=ring(ry,yi,rz,yt-rz)
 add('02_rotor_back_iron',yoke,'rotor','#697984',E['rotor_yoke_material'],'OD16.4 ID15.4; 0.50 radial wall')
 cap=ring(ry,bi,yt,zt-yt)
 cap=fuse(cap,ring(G['cap_spigot_od_mm']/2,G['cap_spigot_id_mm']/2,G['cap_spigot_bottom_z_mm'],yt-G['cap_spigot_bottom_z_mm']))
 cap=fuse(cap,ring(ab,bi,bu+bw,yt-bu-bw))
 add('03_rotor_cap',cap,'rotor','#9aabb7','6061-T6 aluminium','Spigot adhesive joint; shaft hub controls inner-ring spacing. Final fits by bearing supplier.')
 shaft=fuse(cyl(bi,bl,zt-.15-bl),cyl(ab,bl-.3,.3))
 add('04_rotor_shaft',shaft,'rotor','#c1cad0','Nonmagnetic austenitic stainless steel','D2 precision ground seats; integral lower D2.6 thrust collar, magnet bond face')
 pitch=360/G['magnet_poles']
 for i in range(G['magnet_poles']):
  a=i*pitch;half=(pitch-G['magnet_joint_gap_deg'])/2
  s=sector(G['magnet_od_mm']/2,G['magnet_id_mm']/2,a-half,a+half,G['magnet_bottom_z_mm'],G['magnet_height_mm'])
  add(f'05_magnet_{i+1:02d}_{"N" if i%2==0 else "S"}',s,'magnet','#d96159' if i%2==0 else '#4c8bbd',E['magnet_material'],'Pole label is the inward-facing surface; 0.6 degree adhesive joint; independent arcs form a 14-pole ring')
 # Stationary carrier, bearing bores and central outer-ring spacer.
 base=cyl(br,0,bz)
 pcbz=G['sensor_pcb_thickness_mm']
 base=cut(base,cyl(G['sensor_pocket_od_mm']/2,-.01,pcbz+.01));base=cut(base,cyl(bo,pcbz,bz-pcbz+.01))
 post=ring(pr,bo,bz,bu+bw-bz)
 post=fuse(post,ring(bo,stop,bl+bw,bu-bl-bw))
 carrier=fuse(base,post)
 for a in G['mount_angles_deg']:
  x,y=G['mount_pcd_mm']/2*math.cos(math.radians(a)),G['mount_pcd_mm']/2*math.sin(math.radians(a))
  carrier=fuse(carrier,cyl(G['mount_boss_od_mm']/2,bz,G['mount_boss_top_z_mm']-bz,x,y))
  carrier=cut(carrier,cyl(G['mount_pilot_diameter_mm']/2,-.01,G['mount_boss_top_z_mm']-.24,x,y))
 # Recessed flex/wire passage: at 135 degrees, toward the device interior.
 channel=rotate(box(0,-1,0,br+.1,2,pcbz),G['sensor_tail_angle_deg'])
 carrier=cut(carrier,channel)
 # 1.2 mm axial wire passage, separate from the sensor route and bearing bore.
 a=math.radians(90);carrier=cut(carrier,cyl(.45,-.01,bz+.02,(sy+.7)*math.cos(a),(sy+.7)*math.sin(a)))
 add('06_stator_carrier',carrier,'fixed','#aebdc5','6061-T6 aluminium','Three M1.4 blind pilot holes, bearing fits and adhesive retention require tolerance review')
 core=ring(sy,si,sz,sl)
 for i in range(G['slots']):
  tooth=fuse(box(sy-.1,-tw/2,sz,tr-sy+.15,tw,sl),sector(so,tr,-ta,ta,sz,sl))
  core=fuse(core,rotate(tooth,i*360/G['slots']))
 add('07_stator_stack',core,'fixed','#717f88',E['stator_material'],'20 nominal 0.20 mm insulated laminations; final assembled stack 4.00 mm incl. coating')
 colors={'A':'#c9823d','B':'#d0a548','C':'#a96541'}
 for i,phase in enumerate(E['winding_sequence_ccw_from_tooth_1']):
  cr0,cr1=G['coil_r_min_mm'],G['coil_r_max_mm'];cw,ci=G['coil_half_width_mm'],G['coil_inner_half_width_mm']
  winding=box(cr0,-cw,G['coil_bottom_z_mm'],cr1-cr0,2*cw,G['coil_top_z_mm']-G['coil_bottom_z_mm'])
  winding=cut(winding,box(cr0-.01,-ci,sz-.1,cr1-cr0+.02,2*ci,sl+.2))
  add(f'08_coil_{i+1:02d}_{phase[0]}{phase[1].replace("+","p").replace("-","n")}',rotate(winding,i*360/G['slots']),'coil',colors[phase[0]],'Enamelled copper winding envelope',f'{phase}, 32 turns D0.13 Cu, finished OD <=0.145, 11/11/10 layers. Envelope is not solid copper.')
 for i,z in enumerate((bl,bu)):
  add(f'09_bearing_{i+1}',ring(bo,bi,z,bw),'bearing','#c4ced5','EZO MR52ZZ 2 x 5 x 2.5','Envelope only, rolling elements not modelled; zero interference fit in nominal geometry')
 add('10_inner_spacer',ring(ab,bi,bl+bw,bu-bl-bw),'rotor','#c4ced5','Ground nonmagnetic stainless shim','Match inner/outer spacing to avoid unintended preload; select shim after measurement')
 for name,z in [('11_lower_outer_retainer',bl-.2),('12_upper_outer_retainer',bu+bw)]:
  add(name,ring(bo,stop,z,.2),'fixed','#9eacb7','Nonmagnetic stainless steel','Bonded outer-race retainer; no contact with inner ring or shields outside outer-ring land')
 # The upper retainer occupies z7.8..8; rotor cap underside is relieved to clear it.
 # Remove central counterbore in cap, retaining D2.6 rotating hub.
 cap_part=next(p for p in PARTS if p['name']=='03_rotor_cap')
 cap_part['shape']=cut(cap_part['shape'],ring(pr+.2,ab+.1,bu+bw-.01,.31))
 add('13_sensor_magnet',cyl(G['sensor_magnet_od_mm']/2,G['sensor_magnet_bottom_z_mm'],G['sensor_magnet_height_mm']),'rotor','#b088b6','NdFeB, diametrically magnetized','Single pole pair; D4 x 0.5; 0.02 adhesive to shaft collar. Non-reference magnet requires field validation.')
 pcb=fuse(cyl(G['sensor_pcb_od_mm']/2,0,pcbz),rotate(box(0,-.8,0,br-.2,1.6,.15),G['sensor_tail_angle_deg']))
 add('14_sensor_flex',pcb,'sensor','#3b977d','Rigid-flex envelope, 0.4 mm central stiffened island','D5.5 sensor island and 1.6 mm tail to module edge; actual PCB layout not included')
 sx,syy,szc=G['sensor_chip_max_xyz_mm']
 add('15_MT6701_max_envelope',box(-sx/2,-syy/2,pcbz+G['sensor_solder_z_mm'],sx,syy,szc),'sensor','#34434d','MT6701QT-STD QFN16','Maximum 3.1 x 3.1 x 0.8 body plus 0.05 solder allowance')
 add('16_sensor_decoupling',box(1.65,-.15,.45,.6,.3,.3),'sensor','#a18e76','0201 capacitor envelope','100 nF adjacent VDD/GND; land pattern and routing to be designed')

def analysis():
 seq=E['winding_sequence_ccw_from_tooth_1'];p=G['magnet_poles']/2;Q=G['slots']
 phasors={}
 for phase in 'ABC':
  terms=[(1 if s[1]=='+' else -1)*np.exp(1j*p*i*2*np.pi/Q) for i,s in enumerate(seq) if s[0]==phase]
  avg=sum(terms)/len(terms)
  phasors[phase]={'distribution_factor':float(abs(avg)),'electrical_axis_deg':float(np.angle(avg,deg=True))}
 kp=abs(math.sin(math.pi*p/Q));kw=kp*phasors['A']['distribution_factor']
 d=E['wire_copper_diameter_mm'];area=math.pi*d*d/4;turns=E['turns_per_tooth'];N=Q//3*turns
 length=(N*E['mean_turn_length_estimate_mm']+E['phase_extra_lead_length_mm'])/1000
 R=E['copper_resistivity_20C_ohm_mm2_m']*length/area
 out={'method':'Analytical sinusoidal PM flux linkage sensitivity, NOT FEA or measured performance',
 'convention':'Iq is peak phase current; Rphase is one star phase at 20 C; copper loss=1.5*Rphase*Iq^2; Kt=1.5*p*lambda_phase_peak',
 'winding_phasors':phasors,'coil_pitch_factor':kp,'winding_factor':kw,'phase_series_turns':N,'wire_length_per_phase_m':length,
 'phase_resistance_20C_ohm':R,'line_line_resistance_20C_ohm':2*R,
 'phase_resistance_80C_ohm':R*(1+E['copper_temperature_coefficient_per_C']*60),
 'coil_copper_fill_in_allocated_side':turns*area/(1.75*.48),
 'finished_wire_area_fill_in_allocated_side':turns*math.pi*E['wire_max_finished_diameter_mm']**2/4/(1.75*.48),
 'scenarios':[]}
 for B in E['airgap_fundamental_B_scenarios_T']:
  r=(G['stator_od_mm']+G['magnet_id_mm'])/4/1000;l=G['stator_stack_mm']/1000
  phi=2*B*r*l/p;lam=N*kw*phi;kt=1.5*p*lam
  case={'assumed_fundamental_airgap_B_T':B,'Kt_mNm_per_A_peak':kt*1000,'flux_per_pole_uWb':phi*1e6,'points':[]}
  for torque in E['target_detent_mNm']:
   iq=torque/(kt*1000)
   case['points'].append({'torque_mNm':torque,'rim_force_N':torque/8.5,'Iq_peak_A':iq,'phase_RMS_A':iq/math.sqrt(2),'copper_loss_20C_W':1.5*R*iq*iq,'copper_loss_80C_W':1.5*out['phase_resistance_80C_ohm']*iq*iq,'ideal_zero_speed_bus_min_20C_V':math.sqrt(3)*R*iq})
  Ibudget=math.sqrt(E['initial_copper_loss_budget_W']/(1.5*R))
  case['torque_at_unverified_0p5W_copper_budget_mNm']=kt*Ibudget*1000
  out['scenarios'].append(case)
 return out

def to_case(sh):
 t=gp_Trsf();t.SetRotation(gp_Ax1(gp_Pnt(0,0,0),gp_Dir(1,0,0)),-math.pi/2)
 s=BRepBuilderAPI_Transform(sh,t,True).Shape()
 return translate(s,G['case_axis_xz_mm'][0],G['case_base_y_mm'],G['case_axis_xz_mm'][1])

def validate():
 report={'revision':'HW17-R0','parts':[], 'interferences':[], 'clearances':{},'limitations':[
 'Nominal geometry only; no bearing fit, dynamic runout, flex routing, adhesive or deformation simulation.',
 'Windings, bearings, PCB and sensor packages are envelopes; no motor FEM, thermal test, firmware or electrical PCB sign-off.',
 'Separate case mounting adapter and external cable harness remain to be designed; cartridge envelope excludes them.'
 ]}
 for part in PARTS:
  sh=part['shape'];bb=bbox(sh)
  report['parts'].append({k:v for k,v in part.items() if k!='shape'}|{'bbox_mm':bb,'volume_mm3':vol(sh),'valid':BRepCheck_Analyzer(sh).IsValid(),'solid_count':len(solids(sh))})
 for i,a in enumerate(PARTS):
  ba=bbox(a['shape'])
  for b in PARTS[i+1:]:
   bb=bbox(b['shape'])
   if any(min(ba[k+3],bb[k+3])-max(ba[k],bb[k])<1e-7 for k in range(3)):continue
   common=vol(BRepAlgoAPI_Common(a['shape'],b['shape']).Shape())
   if common>1e-6:report['interferences'].append({'a':a['name'],'b':b['name'],'volume_mm3':common})
 allshape=compound([p['shape'] for p in PARTS]);bb=bbox(allshape)
 report['envelope_bbox_mm']=bb;report['axial_height_mm']=bb[5]-bb[2]
 outside=vol(cut(allshape,cyl(8.5,-.001,9.002)))
 report['volume_outside_D17_H9_mm3']=outside
 report['clearances'].update(radial_magnetic_airgap_mm=.25,magnet_yoke_bond_radial_mm=.05,stator_carrier_bond_radial_mm=.05,sensor_magnet_to_max_IC_top_mm=1.88-1.25,coil_to_cap_axial_mm=8-6.88,nominal_cartridge_to_9mm_limit_mm=9-(bb[5]-bb[2]))
 # Actual source geometry, cartridge-only fit; old mechanical encoder is replaced.
 casepath=ROOT/P['integration']['case'];cases=solids(read_step(casepath));module=to_case(allshape)
 report['sources']={str(casepath.relative_to(ROOT)):hashlib.sha256(casepath.read_bytes()).hexdigest()}
 report['fit_checks']=[]
 for name,obstacle in [('V8.3_MainCase',cases[0]),('V8.3_BackCover',cases[8])]:
  common=vol(BRepAlgoAPI_Common(module,obstacle).Shape());d=BRepExtrema_DistShapeShape(module,obstacle);d.Perform()
  report['fit_checks'].append({'obstacle':name,'overlap_mm3':common,'minimum_distance_mm':d.Value()})
 boardpath=ROOT/P['integration']['main_board'];board=read_step(boardpath)
 report['sources'][str(boardpath.relative_to(ROOT))]=hashlib.sha256(boardpath.read_bytes()).hexdigest()
 # Clip to a tight corner box to avoid processing hundreds of unrelated components.
 roi=box(10,1,29,19,12,19);near=BRepAlgoAPI_Common(board,roi).Shape()
 common=vol(BRepAlgoAPI_Common(module,near).Shape());d=BRepExtrema_DistShapeShape(module,board);d.Perform()
 report['fit_checks'].append({'obstacle':'RevB populated board in existing BodyV6.13 datum','overlap_mm3':common,'minimum_distance_mm':d.Value()})
 moving=compound([p['shape'] for p in PARTS if p['group'] in ('rotor','magnet')])
 stationary=compound([p['shape'] for p in PARTS if p['group'] in ('fixed','coil','sensor')])
 d=BRepExtrema_DistShapeShape(moving,stationary);d.Perform()
 report['rotor_to_stationary_minimum_excluding_bearings_mm']=d.Value()
 sweep=to_case(cyl(G['grip_od_mm']/2,G['rotor_bottom_z_mm'],G['top_z_mm']-G['rotor_bottom_z_mm']))
 report['all_angle_external_clearance']=[]
 for name,ob in [('MainCase',cases[0]),('BackCover',cases[8]),('MainBoard',board)]:
  common=vol(BRepAlgoAPI_Common(sweep,ob).Shape());d=BRepExtrema_DistShapeShape(sweep,ob);d.Perform()
  report['all_angle_external_clearance'].append({'name':name,'swept_envelope_overlap_mm3':common,'clearance_mm':d.Value()})
 write_step(OUT/'HW17-R0-cartridge-in-case-frame.step',module)
 write_step(OUT/'HW17-R0-V8.3-fit-context.step',compound([cases[0],cases[8],board,module]))
 return report

def mesh(sh):
 BRepMesh_IncrementalMesh(sh,.06,False,.25,True).Perform();vertices=[];triangles=[]
 e=TopExp_Explorer(sh,TopAbs_FACE)
 while e.More():
  face=TopoDS.Face(e.Current());loc=TopLoc_Location();tri=BRep_Tool.Triangulation_s(face,loc)
  if tri is not None:
   tr=loc.Transformation();offset=len(vertices)
   for i in range(1,tri.NbNodes()+1):
    p=tri.Node(i).Transformed(tr);vertices.append([p.X(),p.Y(),p.Z()])
   for i in range(1,tri.NbTriangles()+1):
    a,b,c=tri.Triangle(i).Get()
    if face.Orientation()==TopAbs_REVERSED:b,c=c,b
    triangles.append([offset+a-1,offset+b-1,offset+c-1])
  e.Next()
 return vertices,triangles

if __name__=='__main__':
 build();(HERE/'analysis.json').write_text(json.dumps(analysis(),indent=2)+'\n')
 for part in PARTS:write_step(OUT/(part['name']+'.step'),part['shape'])
 assembly=compound([p['shape'] for p in PARTS]);write_colored_assembly(OUT/'HW17-R0-assembly.step')
 write_step(OUT/'HW17-R0-rotor.step',compound([p['shape'] for p in PARTS if p['group'] in ('rotor','magnet')]))
 write_step(OUT/'HW17-R0-stator.step',compound([p['shape'] for p in PARTS if p['group'] not in ('rotor','magnet')]))
 print('Checking part intersections and source CAD fit...',flush=True)
 report=validate()
 reopened=read_step(OUT/'HW17-R0-assembly.step')
 report['step_roundtrip']={'valid':BRepCheck_Analyzer(reopened).IsValid(),'solid_count':len(solids(reopened)),'volume_difference_mm3':abs(vol(reopened)-vol(assembly))}
 (HERE/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
 assert not report['interferences'],report['interferences']
 assert report['volume_outside_D17_H9_mm3']<1e-6
 assert all(p['valid'] and p['solid_count']==1 for p in report['parts'])
 assert report['step_roundtrip']['valid'] and report['step_roundtrip']['volume_difference_mm3']<1e-5
 assert all(x['overlap_mm3']<1e-6 for x in report['fit_checks'])
 assert all(x['swept_envelope_overlap_mm3']<1e-6 for x in report['all_angle_external_clearance'])
 meshes=[]
 for p in PARTS:
  v,t=mesh(p['shape']);meshes.append({k:p[k] for k in ('name','group','color')}|{'vertices':v,'triangles':t})
 (HERE/'mesh.json').write_text(json.dumps(meshes,separators=(',',':')))
 with (HERE/'parts.csv').open('w') as f:
  w=csv.writer(f);w.writerow(['part','group','material','note'])
  for p in PARTS:w.writerow([p['name'],p['group'],p['material'],p['note']])
 print(json.dumps({k:report[k] for k in ('interferences','envelope_bbox_mm','volume_outside_D17_H9_mm3','fit_checks','step_roundtrip')},indent=2),flush=True)
