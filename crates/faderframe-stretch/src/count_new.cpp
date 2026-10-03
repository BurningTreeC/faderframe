// Test builds only: count C++ heap allocations so the tests can prove the
// stretcher never allocates while processing (the Rust counting allocator
// cannot see operator new).
#include <atomic>
#include <cstdint>
#include <cstdlib>
#include <new>

static std::atomic<uint64_t> allocations{0};

extern "C" uint64_t ff_stretch_cpp_allocations() { return allocations.load(); }

void *operator new(std::size_t n) {
    allocations.fetch_add(1, std::memory_order_relaxed);
    if (void *p = std::malloc(n ? n : 1)) return p;
    throw std::bad_alloc();
}
void *operator new[](std::size_t n) { return operator new(n); }
void *operator new(std::size_t n, std::align_val_t a) {
    allocations.fetch_add(1, std::memory_order_relaxed);
    if (void *p = std::aligned_alloc(std::size_t(a), (n + std::size_t(a) - 1) / std::size_t(a) * std::size_t(a))) return p;
    throw std::bad_alloc();
}
void *operator new[](std::size_t n, std::align_val_t a) { return operator new(n, a); }
void operator delete(void *p) noexcept { std::free(p); }
void operator delete[](void *p) noexcept { std::free(p); }
void operator delete(void *p, std::size_t) noexcept { std::free(p); }
void operator delete[](void *p, std::size_t) noexcept { std::free(p); }
void operator delete(void *p, std::align_val_t) noexcept { std::free(p); }
void operator delete[](void *p, std::align_val_t) noexcept { std::free(p); }
void operator delete(void *p, std::size_t, std::align_val_t) noexcept { std::free(p); }
void operator delete[](void *p, std::size_t, std::align_val_t) noexcept { std::free(p); }
