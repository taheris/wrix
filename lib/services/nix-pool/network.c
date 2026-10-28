#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_endian.h>

/* Attachment to the owned work subtree applies across native UID mappings. */
static __always_inline int public_ipv4(__u32 address)
{
    __u32 ip = bpf_ntohl(address);
    if ((ip >> 24) == 0 || (ip >> 24) == 10 || (ip >> 24) == 127)
        return 0;
    if ((ip & 0xffc00000) == 0x64400000 ||
        (ip & 0xffff0000) == 0xa9fe0000 ||
        (ip & 0xfff00000) == 0xac100000 ||
        (ip & 0xffffff00) == 0xc0000000 ||
        (ip & 0xffffff00) == 0xc0000200 ||
        (ip & 0xffffff00) == 0xc0586300 ||
        (ip & 0xffff0000) == 0xc0a80000 ||
        (ip & 0xfffe0000) == 0xc6120000 ||
        (ip & 0xffffff00) == 0xc6336400 ||
        (ip & 0xffffff00) == 0xcb007100 ||
        (ip & 0xf0000000) == 0xe0000000 ||
        (ip & 0xf0000000) == 0xf0000000)
        return 0;
    return 1;
}

SEC("cgroup/connect4")
int connect4(struct bpf_sock_addr *context)
{
    return public_ipv4(context->user_ip4);
}

SEC("cgroup/sendmsg4")
int sendmsg4(struct bpf_sock_addr *context)
{
    return public_ipv4(context->user_ip4);
}

SEC("cgroup/connect6")
int connect6(struct bpf_sock_addr *context)
{
    (void)context;
    return 0;
}

SEC("cgroup/sendmsg6")
int sendmsg6(struct bpf_sock_addr *context)
{
    (void)context;
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
