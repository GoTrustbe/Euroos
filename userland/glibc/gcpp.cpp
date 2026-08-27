#include <cstdio>
#include <vector>
#include <string>
#include <algorithm>
#include <stdexcept>
#include <unistd.h>
static int throw_and_catch(){
    try { throw std::runtime_error("boom"); }
    catch(const std::exception& e){ return e.what()[0]=='b' ? 1 : 0; } // 'b' from "boom"
}
int main(){
    std::vector<int> v{5,3,9,1,7,2};           // heap alloc (operator new)
    std::sort(v.begin(), v.end());             // <algorithm>
    std::string s;                             // std::string
    for(int x : v){ s += std::to_string(x); s += ' '; }
    int caught = throw_and_catch();            // C++ exception unwinding (libgcc_s)
    bool sorted = std::is_sorted(v.begin(), v.end());
    printf("GCPP: sorted='%s' is_sorted=%d exc_caught=%d\n", s.c_str(), sorted, caught);
    bool ok = sorted && caught && s=="1 2 3 5 7 9 ";
    printf("GCPP: %s\n", ok ? "PASS":"FAIL");
    fflush(stdout);
    _exit(ok ? 66 : 1);
}
