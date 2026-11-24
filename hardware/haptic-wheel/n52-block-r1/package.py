"""Cross-check cut geometry, write supplier text/inspection forms, package RFQ deliverables."""
from pathlib import Path
import math, json, csv, hashlib, zipfile, shutil
import ezdxf
from pypdf import PdfReader
import build as cad

HERE=cad.HERE;ROOT=cad.ROOT;REV=cad.REV

def check_lamination():
 d=ezdxf.readfile(cad.DXF/f'{REV}-P06-cut-profile-1to1.dxf')
 assert d.units==4
 edges=[];circles=[]
 def key(p):return tuple(round(float(x),7) for x in p)
 def pt(r,a):return (r*math.cos(a),r*math.sin(a))
 for ent in d.modelspace():
  typ=ent.dxftype()
  if typ=='CIRCLE':circles.append(ent.dxf.radius);continue
  if typ=='LINE':
   p,q=tuple(ent.dxf.start)[:2],tuple(ent.dxf.end)[:2];integral=(p[0]*q[1]-p[1]*q[0])/2
  else:
   assert typ=='ARC';r=ent.dxf.radius;a=math.radians(ent.dxf.start_angle);delta=math.radians((ent.dxf.end_angle-ent.dxf.start_angle)%360)
   p,q=pt(r,a),pt(r,a+delta);integral=.5*r*r*delta
  edges.append((key(p),key(q),integral))
 assert circles==[3.05]
 adj={}
 for i,(p,q,_) in enumerate(edges):adj.setdefault(p,[]).append(i);adj.setdefault(q,[]).append(i)
 assert all(len(x)==2 for x in adj.values())
 start=edges[0][0];pos=start;used=set();area=0
 while len(used)<len(edges):
  choices=[x for x in adj[pos] if x not in used];assert choices
  i=choices[0];p,q,a=edges[i];used.add(i)
  if pos==p:pos=q;area+=a
  else:pos=p;area-=a
 assert pos==start
 area=abs(area)-math.pi*circles[0]**2
 shape=cad.read_step(cad.CAD/f'{REV}-P06.step');cad_area=cad.vol(shape)/.2
 assert abs(area-cad_area)<1e-6,(area,cad_area)
 return {'dxf_units':'mm','outer_curve_entities':len(edges),'inner_circles':1,'closed_outer_loop':True,'dxf_area_mm2':area,'step_area_mm2':cad_area,'difference_mm2':abs(area-cad_area)}

def plate_dxf():
 d=ezdxf.new('R2010');d.units=4
 for name,color in [('THROUGH_CUT',7),('TOP_POCKET_DEPTH_0p45',3)]:d.layers.new(name,dxfattribs={'color':color})
 m=d.modelspace();a={'layer':'THROUGH_CUT'}
 m.add_lwpolyline([(-20,-15),(20,-15),(20,15),(-20,15)],close=True,dxfattribs=a)
 m.add_circle((0,0),4,dxfattribs=a)
 for ang in (30,150,270):m.add_circle((5.4*math.cos(math.radians(ang)),5.4*math.sin(math.radians(ang))),.8,dxfattribs=a)
 for x in (-16,16):
  for y in (-11,11):m.add_circle((x,y),1.7,dxfattribs=a)
 # Finished groove boundaries clipped at Y=15; central opening already through-cut.
 a={'layer':'TOP_POCKET_DEPTH_0p45'};theta=math.radians(120);u=(math.cos(theta),math.sin(theta));n=(-u[1],u[0])
 for sign in (-1,1):
  p=(sign*n[0],sign*n[1]);t=(15-p[1])/u[1]
  m.add_line(p,(p[0]+u[0]*t,15),dxfattribs=a)
 d.saveas(cad.DXF/f'{REV}-J02-plate-1to1.dxf')

def supplier_files():
 rfq='''供应商您好，请按附件 HW18-B1 资料报价微型力反馈电机台架样机。

一、项目与版本
采用用户自备14片N52 4×3×1mm标准方块磁铁；12槽/14极；定子Φ13×4。
名义本体Φ18×8.8mm，验收外包络最大Φ18.05×9.00mm。本版为台架样机，允许大于原Φ17机壳；不是原机壳直接替换件。
这是样机询价资料，非已验证性能的量产图纸。我们需要工艺可行性、可达公差与价格反馈。

二、请分别报价1、5、10台的数量阶梯
A. 车铣/磨削：P01~P05；J01粘磁工装每批1件；J02台架底板按每台1件另列。
B. 精密薄片：P07内圈选配隔垫、P08外圈隔垫、P09挡环。
   P07每台实际使用1片，首批0.08/0.09/0.10/0.11/0.12五档厚度各另备2片；备件费用单列。
C. 定子：P06切割散片、粘叠铁芯、A02绕线定子三个供货层级分别报价。A02包含P06，不能重复计费。
   单片名义0.20mm，20片只是参考，最终成叠厚4.00±0.03mm含涂层和胶；必须保留片间绝缘。
D. 采购件：C02两只MR52ZZ；C03一只Φ4×0.5、直径方向两极充磁的N35SH测角磁铁。
   C03优先现货；需定制时请单列MOQ、样品费与交期。C01是用户自备，不报价定制磁瓦。
E. 如能代装，请单列磁铁粘接、轴承选配、总成装配及检验费用。电子板/固件不在本机械报价范围。

三、特别需要确认的加工及检测能力
1. 钢轭每个磁铁平面距轴8.000±0.015，十四面位置角±0.05°；外圆与定位孔跳动≤0.02。
2. 转轴轴承段Φ2 -0.003/-0.006，Ra≤0.4；配实际轴承目标直径间隙0.002~0.006。
3. 固定座轴承孔Φ5 +0.008/+0.003，圆度≤0.003，Ra≤0.4；配轴承外圈目标间隙0.003~0.012。
4. P05的3个M1.4×0.3-6H盲螺孔，完整牙深1.00~1.10、底孔总深≤1.40，不能穿透凸台。
5. P07/P08薄隔垫厚度公差±0.003，平面度≤0.005；提供实测厚度并分袋标识。
6. 叠片切边毛刺≤0.010、片间绝缘、成叠跳动≤0.03；绕线每齿32匝，严格按A02齿序。
7. 装配全周最小磁气隙≥0.25、轴向游隙0.02~0.05、相邻磁铁实体间距≥0.03。
8. 磁铁来料允许4/3/1三个尺寸各±0.05，逐片测厚、测宽和查极性；超范围须反馈。用J01和PET补偿片控制背胶层0.03~0.07。

四、报价反馈格式
请逐件列明：图号、数量阶梯、材料牌号、加工路线、可达公差、单价、工装费、检验费、交期、税运费。
无法实现或建议调整的尺寸/材料，请逐条写出“原要求 / 可实现值 / 成本或交期影响”。不要默认按一般公差放宽。
首件请提供全尺寸检测记录；关键尺寸100%检验。材料替代、胶种/底涂与固化温度也请列明。
PDF明确尺寸和要求优先；STEP是名义实体；DXF是mm/1:1工艺轮廓。一般倒钝与螺纹牙型按PDF，未全部实体化。
此询价不构成采购订单。
'''
 (HERE/'供应商询价文字.txt').write_text(rfq)
 readme='''HW18-B1 N52 方块磁铁电机 - 外发询价包
版本：B1，2026-10-30

先读 PDF/HW18-B1_N52方块磁铁电机_询价图纸及加工要求.pdf。
本包是台架样机：名义Φ18×8.8，验收最大Φ18.05×9.00。用户已允许为台架验证放宽原Φ17限制。
现货电机磁铁：4轴向×3切向×1径向、N52、沿1mm充磁，共14片。尺寸接收范围各±0.05尚需来料实测，不代表已测量用户实物。

文件目录
STEP/  - 逐件名义模型，以及带分件颜色/名称的A01总装、转子、固定件、台架与粘磁工装装载参考。
         A02.step为成叠铁芯实体；A02-winding-envelope.step另含12个绕线包络，供绕线厂校核空间。
PDF/   - 逐件加工图及总图册；P05与A02各两页。所有明确公差、基准、材料、表面与加工说明在PDF。
DXF/   - mm单位、1:1名义轮廓。P06与P07~09为闭合切割轮廓；P02/J01为截面参考；J02绿色层是0.45深顶面槽，不能通切。
images/ - 由真实实体生成的剖切预览。
BOM_询价清单.csv - 图号、名称、数量、材料与工艺。A02已包含P06；C01用户自备；P07备用厚度另计；J01共用。
供应商询价文字.txt - 可复制给供应商的中文询价说明。
检验记录模板.csv / 报价反馈模板.csv - 供供应商填写；不是已经完成的检验结果。
verification/ - CAD、STEP回读、干涉/连续转动扫掠、DXF与PDF校验结果。
source/ - 可复现源代码（压缩包内）。

适用范围与限制
1. 不包含原机壳改造、转接支架、PCB制造文件、外部线束或固件。电子包络不可用于制板。
2. 普通N52不同于N52H/SH；先短时限流，磁铁附近50℃为初始停测点。未验证额定力矩、持续温升、退磁裕量、寿命或编码器串扰。
3. 独立C03测角磁铁仍为Φ4×0.5直径方向两极充磁，与C01完全不同。若不是现货需单列定制询价。
4. 轴承型号虽然确定，配合仍按实际批次内外径分组选配；无法达到图纸配合时供应商须报偏差。
5. J01的核心对边13.80，1.00厚磁铁使用0.05PET片；每片按s=1.05-t实测选配。胶后取净PET。
6. 一般倒钝和螺纹牙型按二维要求实施，未全部建入STEP；其他主体形状均应与模型一致。
7. PDF有实际尺寸、公差与检验指标，可直接用于询价；正式加工前应将供应商反馈的偏差纳入版本管理。

复现（在项目根目录）
target/hw17-r1-venv/bin/python hardware/haptic-wheel/n52-block-r1/build.py
target/hw17-r1-venv/bin/python hardware/haptic-wheel/n52-block-r1/drawings.py
target/hw17-r1-venv/bin/python hardware/haptic-wheel/n52-block-r1/package.py
依赖：Python3.12、cadquery-ocp、numpy、ezdxf、reportlab、pypdf、Pillow；中文字体使用macOS STHeiti。
PDF必须另行渲染检查；自动包围框检查不能替代图面复核。
'''
 (HERE/'请先阅读.txt').write_text(readme)
 with (HERE/'检验记录模板.csv').open('w',encoding='utf-8-sig',newline='') as f:
  w=csv.writer(f);w.writerow(['批次/样机号','图号','检验项目','要求','实测值','方法/量具','温度','结论','检验员','备注'])
  items=[('P02','14平面距离及角度','8.000±0.015；位置角±0.05°'),('P02','定位孔/外圆跳动','16.650~16.665；跳动≤0.02'),('P04','轴径和实际轴承内径','轴1.994~1.997；选配间隙0.002~0.006'),('P05','座孔和实际轴承外径','孔5.003~5.008；选配间隙0.003~0.012'),('P05','圆度/下挡肩/盲螺孔','按P05两页'),('P07','选用内圈隔垫厚度','0.08~0.12选配；每档±0.003'),('P08','外圈隔垫厚度','0.100±0.003'),('A02','成叠尺寸','OD13±0.03；厚4±0.03'),('A02','U-V/V-W/W-U电阻','20±2℃三组差值/平均≤5%'),('A02','匝数/绕向/绝缘','每齿32；100V DC对芯≥20MΩ'),('A01','最小全周气隙','≥0.25'),('A01','轴向游隙','0.02~0.05，±0.2N轻载'),('A01','外圈径向/端盖轴向跳动','≤0.08 / ≤0.05'),('A01','磁铁间距','全部≥0.03'),('A01','测角磁铁偏心/到芯片间隙','偏心≤0.05；间隙≥0.50'),('A01','外包络','≤Φ18.05×9.00'),('A01','胶种/底涂/固化','实录批次与工艺；不得高温损伤N52'),('A01','功能/电流/温升','记录条件；初始50℃停测，不是额定性能验收')]
  for pid,key,req in items:w.writerow(['',REV+'-'+pid,key,req,'','','','','',''])
  for i in range(14):w.writerow(['',f'{REV}-C01-{i+1:02d}','三向尺寸/厚度/朝内极性',f'4/3/1各±0.05；内面{"N" if i%2==0 else "S"}；PET=1.05-t','','','','','',''])
 with (HERE/'报价反馈模板.csv').open('w',encoding='utf-8-sig',newline='') as f:
  w=csv.writer(f);w.writerow(['图号/范围','数量阶梯','材料牌号','工艺','单价','工装费','检验费','交期','原公差要求','可实现值/偏差','其他备注'])
  for pid in ['P01','P02','P03','P04','P05','P06散片','P06成叠','A02绕线组件','P07及备用厚度','P08','P09','C02','C03','J01','J02','总装及检验']:
   for q in ([1] if pid=='J01' else [1,5,10]):w.writerow([REV+'-'+pid,q,'','','','','','','','',''])

def archive():
 dest=ROOT/'output/HW18-B1_N52方块磁铁电机_外发询价包.zip'
 files=[]
 for folder in ['STEP','PDF','DXF','images']:
  files.extend(p for p in (HERE/folder).rglob('*') if p.is_file())
 files.extend(HERE/name for name in ['BOM_询价清单.csv','检验记录模板.csv','报价反馈模板.csv','供应商询价文字.txt','请先阅读.txt','design.json','catalog.json'])
 files.extend(p for p in cad.QA.glob('*.json') if p.name not in ('mesh.json','text-boxes.json'))
 sources=[HERE/n for n in ['build.py','drawings.py','package.py']]
 manifest={str(p.relative_to(HERE)):hashlib.sha256(p.read_bytes()).hexdigest() for p in files}
 for p in sources:manifest['source/'+p.name]=hashlib.sha256(p.read_bytes()).hexdigest()
 (HERE/'MANIFEST.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2))
 with zipfile.ZipFile(dest,'w',zipfile.ZIP_DEFLATED) as z:
  for p in files:z.write(p,'HW18-B1/'+str(p.relative_to(HERE)))
  for p in sources:z.write(p,'HW18-B1/source/'+p.name)
  z.write(HERE/'MANIFEST.json','HW18-B1/MANIFEST.json')
 with zipfile.ZipFile(dest) as z:
  assert z.testzip() is None
  for path,sha in manifest.items():assert hashlib.sha256(z.read('HW18-B1/'+path)).hexdigest()==sha
 return {'path':str(dest),'files':len(manifest)+1,'bytes':dest.stat().st_size,'sha256':hashlib.sha256(dest.read_bytes()).hexdigest()}

if __name__=='__main__':
 report=check_lamination();plate_dxf();supplier_files()
 (cad.QA/'dxf-check.json').write_text(json.dumps(report,indent=2))
 print(json.dumps(report,indent=2))
 result=archive();print(json.dumps(result,ensure_ascii=False,indent=2))
