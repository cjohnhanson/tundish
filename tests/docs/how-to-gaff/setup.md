The guide assumes the subscription from the getting-started tutorial.

```console
$ tundish subscribe inbox --as reviewer --queue slack --consume on-ack
created inbox
bound reviewer to inbox
rules: {"queue":"slack","select":"since_last_read","consume":"on_ack","share":"all","lease":600,"max_attempts":3,"start":"all"}

```

