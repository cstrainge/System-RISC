// Original System7.6.1 Standard File observations; no expected results.
#include <Quickdraw.h>
#include <Fonts.h>
#include <Windows.h>
#include <Menus.h>
#include <Dialogs.h>
#include <StandardFile.h>
#include <Files.h>
#include <cstdio>
#include <cstring>
static void ps(const char* in,unsigned char* out){out[0]=std::strlen(in);std::memcpy(out+1,in,out[0]);}
int main(){
 InitGraf(&qd.thePort);InitFonts();InitWindows();InitMenus();InitDialogs(nullptr);TEInit();
 unsigned char volumeName[256];short volume=0;GetVol(volumeName,&volume);*((short*)0x214)=-volume;*((long*)0x398)=2;
 char path[256];std::snprintf(path,sizeof(path),"%.*s:System Folder:Startup Items:StandardFile.jsonl",volumeName[0],volumeName+1);unsigned char name[256];ps(path,name);Create(name,volume,'SFPR','TEXT');short log=0;FSOpen(name,volume,&log);SetEOF(log,0);
 SFTypeList types={'TEXT',0,0,0};StandardFileReply reply;
 StandardGetFile(nullptr,1,types,&reply);
 char row[256];long length=std::snprintf(row,sizeof(row),"{\"open_good\":%d,\"type\":%lu,\"folder\":%d,\"volume\":%d}\r",reply.sfGood,(unsigned long)reply.sfType,reply.sfIsFolder,reply.sfIsVolume);FSWrite(log,&length,row);FlushVol(nullptr,volume);
 unsigned char prompt[256],def[256];ps("Save JSON as:",prompt);ps("resources.json",def);
 StandardPutFile(prompt,def,&reply);
 length=std::snprintf(row,sizeof(row),"{\"save_good\":%d,\"replacing\":%d}\r",reply.sfGood,reply.sfReplacing);FSWrite(log,&length,row);FSClose(log);FlushVol(nullptr,volume);return 0;
}
