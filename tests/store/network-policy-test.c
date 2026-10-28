#include <assert.h>
#include <arpa/inet.h>
#include <stddef.h>

int main(void)
{
    const unsigned int denied[] = {
        0x00000000, 0x0a000001, 0x64400001, 0x647fffff,
        0x7f000001, 0xa9fe0102, 0xac100001, 0xac1fffff,
        0xc0000001, 0xc0000201, 0xc0586301, 0xc0a80101,
        0xc6120001, 0xc613ffff, 0xc6336401, 0xcb007101,
        0xe0000001, 0xefffffff, 0xf0000001, 0xffffffff,
    };
    const unsigned int allowed[] = {
        0x01010101, 0x08080808, 0x643fffff, 0x64800000,
        0xac0fffff, 0xac200000, 0xc611ffff, 0xc6140000,
    };
    struct bpf_sock_addr context = {0};
    for (size_t i = 0; i < sizeof(denied) / sizeof(denied[0]); i++) {
        context.user_ip4 = htonl(denied[i]);
        assert(connect4(&context) == 0);
        assert(sendmsg4(&context) == 0);
    }
    for (size_t i = 0; i < sizeof(allowed) / sizeof(allowed[0]); i++) {
        context.user_ip4 = htonl(allowed[i]);
        assert(connect4(&context) == 1);
        assert(sendmsg4(&context) == 1);
    }
    assert(connect6(&context) == 0);
    assert(sendmsg6(&context) == 0);
    return 0;
}
