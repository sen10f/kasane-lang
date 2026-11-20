// 組み込みの lld を C から呼ぶ薄い殻 (L3)。`lld_link(flavor, argc, argv, &out, &err)`。
// flavor: 0 = ELF (gnu)、1 = wasm、2 = MinGW (COFF)。戻り値は終了コード (0 が成功)。out / err は malloc した文字列 (呼び出し側が free)。
#include "lld/Common/Driver.h"
#include "llvm/Support/raw_ostream.h"
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include "llvm/Passes/PassPlugin.h"

// libLLVMExtensions が Polly の入口を参照するが、-dev に Polly の静的ライブラリが無いので、何もしない版を置く。
llvm::PassPluginLibraryInfo getPollyPluginInfo() { return {LLVM_PLUGIN_API_VERSION, "Polly", "stub", [](llvm::PassBuilder &) {}}; }

LLD_HAS_DRIVER(elf)
LLD_HAS_DRIVER(wasm)
LLD_HAS_DRIVER(mingw)
LLD_HAS_DRIVER(macho)

static char *dup(const std::string &s) {
  char *p = (char *)malloc(s.size() + 1);
  memcpy(p, s.c_str(), s.size() + 1);
  return p;
}

extern "C" int ksn_lld_link(int flavor, int argc, const char **argv, char **out, char **err) {
  std::string so, se;
  llvm::raw_string_ostream os(so), es(se);
  std::vector<const char *> args(argv, argv + argc);
  lld::DriverDef drv = flavor == 1   ? lld::DriverDef{lld::Wasm, &lld::wasm::link}
                     : flavor == 2 ? lld::DriverDef{lld::MinGW, &lld::mingw::link}
                     : flavor == 3 ? lld::DriverDef{lld::Darwin, &lld::macho::link}
                                   : lld::DriverDef{lld::Gnu, &lld::elf::link};
  // 同じプロセスで何度も呼ぶので、状態の後始末をさせる (`exitEarly` = false)。
  lld::Result r = lld::lldMain(args, os, es, {drv});
  os.flush();
  es.flush();
  *out = dup(so);
  *err = dup(se);
  return r.retCode;
}

// libLLVMWindowsManifest (lldCOFF が引く) が使う libxml2 の記号。MinGW のリンクでは manifest を統合しないので呼ばれない。
// 静的な libxml2 に依存しないよう、何もしない版を置く。
extern "C" {
void (*xmlFree)(void *) = [](void *) {};
#define KSN_XML_STUB(name) void *name(...) { return nullptr; }
KSN_XML_STUB(xmlAddChild)
KSN_XML_STUB(xmlCopyNamespace)
KSN_XML_STUB(xmlDocDumpFormatMemoryEnc)
KSN_XML_STUB(xmlDocGetRootElement)
KSN_XML_STUB(xmlDocSetRootElement)
KSN_XML_STUB(xmlFreeDoc)
KSN_XML_STUB(xmlFreeNode)
KSN_XML_STUB(xmlFreeNs)
KSN_XML_STUB(xmlNewDoc)
KSN_XML_STUB(xmlNewNs)
KSN_XML_STUB(xmlNewProp)
KSN_XML_STUB(xmlReadMemory)
KSN_XML_STUB(xmlSetGenericErrorFunc)
KSN_XML_STUB(xmlStrdup)
KSN_XML_STUB(xmlUnlinkNode)
}
