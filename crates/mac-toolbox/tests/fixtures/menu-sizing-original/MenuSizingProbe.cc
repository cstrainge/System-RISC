// Original System7.6.1 CalcMenuSize measurements; no expected results here.
#include <Quickdraw.h>
#include <Fonts.h>
#include <Windows.h>
#include <Menus.h>
#include <Files.h>
#include <cstdio>
#include <cstring>
static void to_pstring(const char* value, unsigned char* output) {
 output[0]=std::strlen(value);std::memcpy(output+1,value,output[0]);
}
int main() {
 InitGraf(&qd.thePort);InitFonts();InitWindows();InitMenus();
 unsigned char path[256];to_pstring(":MenuWidths.jsonl",path);
 Create(path,0,'MNSZ','TEXT');short file=0;if(FSOpen(path,0,&file))return 1;SetEOF(file,0);
 struct Case {const char* name;const char* label;char key;const char* extra;};
 const Case cases[]={
  {"keyless_close","Close Window",0,nullptr},
  {"close_W","Close Window",'W',nullptr},
  {"folder_N","New Folder",'N',nullptr},
  {"short_I","i",'I',nullptr},{"short_W","i",'W',nullptr},
  {"finder_window","Close Window",'W',"Print Window\311"},
  {"finder_desktop","Close Window",'W',"Print Desktop\311"},
  {"long_keyless","Unclipped keyless label",0,nullptr}};
 for(const auto& c:cases) {
  unsigned char title[256];to_pstring("Test",title);auto menu=NewMenu(256,title);
  unsigned char label[256];to_pstring(c.label,label);AppendMenu(menu,label);
  if(c.key)SetItemCmd(menu,1,c.key);
  if(c.extra){to_pstring(c.extra,label);AppendMenu(menu,label);}
  CalcMenuSize(menu);
  char row[256];long n=std::snprintf(row,sizeof(row),"{\"case\":\"%s\",\"width\":%d,\"height\":%d}\r",c.name,(**menu).menuWidth,(**menu).menuHeight);
  FSWrite(file,&n,row);DisposeMenu(menu);
 }
 const char done[]="{\"complete\":true}\r";long n=sizeof(done)-1;FSWrite(file,&n,done);
 FSClose(file);FlushVol(nullptr,0);return 0;
}
